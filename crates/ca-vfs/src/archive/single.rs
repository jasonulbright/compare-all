//! Formats that wrap exactly one file in one compressed stream.

use std::io::{Read, Seek, SeekFrom};

use super::ArchiveBacking;
use crate::cancel::Cancel;
use crate::detect::ArchiveFormat;
use crate::entry::VfsEntry;
use crate::error::{VfsError, VfsResult};
use crate::fs::OpenFile;
use crate::limits::{materialize, Budget, Limits};
use crate::path::VfsPath;

/// Wrap `reader` in the decoder for one compressed stream.
///
/// # Errors
/// Returns [`VfsError::Unsupported`] when `format` is not a single stream.
pub(crate) fn decoder<'a, R: Read + 'a>(
    format: ArchiveFormat,
    reader: R,
) -> VfsResult<Box<dyn Read + 'a>> {
    match format {
        ArchiveFormat::Gz | ArchiveFormat::TarGz => {
            Ok(Box::new(flate2::read::MultiGzDecoder::new(reader)))
        }
        ArchiveFormat::Bz2 | ArchiveFormat::TarBz2 => {
            Ok(Box::new(bzip2_rs::DecoderReader::new(reader)))
        }
        ArchiveFormat::Xz | ArchiveFormat::TarXz => Ok(Box::new(
            lzma_rust2::XzReader::new_mem_limit(reader, true, super::span::DICTIONARY_LIMIT_KIB),
        )),
        other => Err(VfsError::unsupported(format!(
            "{} is not a single compressed stream",
            other.label()
        ))),
    }
}

/// A container holding one compressed file.
#[derive(Debug, Clone)]
pub(crate) struct SingleStream {
    format: ArchiveFormat,
    inner_name: String,
}

impl SingleStream {
    /// The entry inside `archive_name`, named by stripping the compression
    /// suffix.
    pub(crate) fn new(format: ArchiveFormat, archive_name: &str) -> Self {
        Self {
            format,
            inner_name: inner_name(archive_name),
        }
    }

    /// The single entry this container holds.
    pub(crate) fn list(&self, backing: &ArchiveBacking) -> VfsResult<Vec<VfsEntry>> {
        let path = VfsPath::parse(&self.inner_name)?;
        let (size, exact) = self.expanded_size(backing)?;
        let mut entry = VfsEntry::file(path, size);
        entry.size_is_exact = exact;
        if !exact {
            entry.error = Some(
                "size is taken from the container's own record and has not been checked against \
                 the content"
                    .to_owned(),
            );
        }
        Ok(vec![entry])
    }

    /// Expand and count, because only gzip records the expanded size.
    ///
    /// The count runs against a private budget so it does not spend the one
    /// the caller's reads draw on. The second value is false when the size is
    /// the container's own claim rather than a count of what came out.
    fn expanded_size(&self, backing: &ArchiveBacking) -> VfsResult<(u64, bool)> {
        if self.format == ArchiveFormat::Gz {
            if let Some(size) = gzip_isize(backing)? {
                return Ok((size, false));
            }
        }
        let limits = Limits::default();
        let budget = Budget::new(limits.max_archive_bytes);
        let reader = backing.reader()?;
        let mut decoder = decoder(self.format, reader)?;
        let mut counted = 0u64;
        let mut chunk = vec![0u8; 64 * 1024];
        let cancel = Cancel::new();
        let mut limited =
            crate::limits::LimitedReader::new(&mut decoder, 0, limits, budget.clone(), cancel);
        let mut complete = true;
        loop {
            // A container that cannot be expanded still lists; the entry is
            // reported with the bytes counted before the failure, which is a
            // floor rather than the size.
            let Ok(read) = limited.read(&mut chunk) else {
                complete = false;
                break;
            };
            if read == 0 {
                break;
            }
            counted = counted.saturating_add(read as u64);
        }
        Ok((counted, complete))
    }

    /// Open the single entry.
    pub(crate) fn open_entry(
        &self,
        backing: &ArchiveBacking,
        path: &VfsPath,
        limits: &Limits,
        budget: &Budget,
        cancel: &Cancel,
    ) -> VfsResult<OpenFile> {
        if path.as_str() != self.inner_name {
            return Err(VfsError::NotFound { path: path.clone() });
        }
        let reader = backing.reader()?;
        let decoder = decoder(self.format, reader)?;
        materialize(decoder, 0, limits, budget, cancel)
    }
}

/// The name of the file inside a single-stream container.
fn inner_name(archive_name: &str) -> String {
    let lower = archive_name.to_ascii_lowercase();
    for suffix in [".gz", ".bz2", ".bz", ".xz"] {
        if lower.ends_with(suffix) {
            let cut = archive_name.len() - suffix.len();
            let stem = archive_name.get(..cut).unwrap_or_default();
            if !stem.is_empty() {
                return sanitize(stem);
            }
        }
    }
    if archive_name.is_empty() {
        return "content".to_owned();
    }
    format!("{}.out", sanitize(archive_name))
}

/// Reduce a name to one usable component.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '\0' => '_',
            other if (other as u32) < 0x20 => '_',
            other => other,
        })
        .collect();
    let trimmed = cleaned.trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        "content".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The expanded size gzip records in its footer, modulo four gibibytes.
///
/// The field describes the last member only and nothing verifies it, so a
/// stream holding several members, or one whose footer has been edited, makes
/// it wrong. It is reported as a hint the entry marks as unproven, never as
/// the size, because a comparison that ruled two files different on it would
/// be acting on a number the container supplied and no one checked.
fn gzip_isize(backing: &ArchiveBacking) -> VfsResult<Option<u64>> {
    let mut reader = backing.reader()?;
    let end = reader.seek(SeekFrom::End(0))?;
    if end < 18 {
        return Ok(None);
    }
    reader.seek(SeekFrom::End(-4))?;
    let mut footer = [0u8; 4];
    reader.read_exact(&mut footer)?;
    let size = u64::from(u32::from_le_bytes(footer));
    if end > u64::from(u32::MAX) {
        return Ok(None);
    }
    Ok(Some(size))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn inner_name_strips_the_compression_suffix() {
        assert_eq!(inner_name("notes.txt.gz"), "notes.txt");
        assert_eq!(inner_name("notes.txt.BZ2"), "notes.txt");
        assert_eq!(inner_name("plain"), "plain.out");
        assert_eq!(inner_name(".gz"), ".gz.out");
    }

    /// An xz stream whose one block names a 512 MiB LZMA2 dictionary. The
    /// block data after the header is absent.
    fn xz_with_a_large_dictionary() -> Vec<u8> {
        let mut bytes = vec![0xFD, b'7', b'z', b'X', b'Z', 0x00, 0x00, 0x01];
        bytes.extend_from_slice(&crc32fast::hash(&[0x00, 0x01]).to_le_bytes());
        let header = [0x02, 0x00, 0x21, 0x01, 34, 0x00, 0x00, 0x00];
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
        bytes
    }

    #[test]
    fn an_xz_header_naming_a_dictionary_past_the_limit_is_refused() {
        for format in [ArchiveFormat::Xz, ArchiveFormat::TarXz] {
            let mut reader =
                decoder(format, std::io::Cursor::new(xz_with_a_large_dictionary())).unwrap();
            let mut sink = Vec::new();
            let error = reader.read_to_end(&mut sink).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::OutOfMemory, "{error}");
        }
    }

    #[test]
    fn sanitize_removes_separators_and_trailing_dots() {
        assert_eq!(sanitize("a/b:c"), "a_b_c");
        assert_eq!(sanitize("name."), "name");
        assert_eq!(sanitize("..."), "content");
    }
}
