//! Entries whose content is one or more byte ranges of a seekable source.
//!
//! Disc images, package payloads and the tars inside a Debian package all end
//! up here: the listing records, for each file, which source holds it and
//! where, and a read seeks to those ranges. A compressed payload is decoded
//! once into a temporary file under the expansion ceilings, so a later read
//! seeks instead of decoding again.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use super::ArchiveBacking;
use crate::cancel::Cancel;
use crate::error::{VfsError, VfsResult};
use crate::fs::OpenFile;
use crate::limits::{materialize, uncarry_or, Budget, LimitedReader, Limits};
use crate::path::VfsPath;

/// Memory an LZMA or xz decoder may claim for its dictionary, in KiB.
///
/// The dictionary size is a field of the stream, so without a ceiling a
/// crafted header makes the decoder allocate whatever it names.
pub(crate) const DICTIONARY_LIMIT_KIB: u32 = 256 * 1024;

/// How a payload is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compression {
    /// Stored as is.
    None,
    /// Gzip.
    Gzip,
    /// Bzip2.
    Bzip2,
    /// Xz.
    Xz,
    /// Legacy LZMA alone.
    Lzma,
    /// Zstandard, which no decoder in this build reads.
    Zstd,
}

impl Compression {
    /// Wrap `reader` in the decoder for this compression.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] for a compression this build has no
    /// decoder for, and [`VfsError::Corrupt`] when an LZMA header is refused.
    pub(crate) fn decoder<'a, R: Read + 'a>(self, reader: R) -> VfsResult<Box<dyn Read + 'a>> {
        match self {
            Self::None => Ok(Box::new(reader)),
            Self::Gzip => Ok(Box::new(flate2::read::MultiGzDecoder::new(reader))),
            Self::Bzip2 => Ok(Box::new(bzip2_rs::DecoderReader::new(reader))),
            Self::Xz => Ok(Box::new(lzma_rust2::XzReader::new_mem_limit(
                reader,
                true,
                DICTIONARY_LIMIT_KIB,
            ))),
            Self::Lzma => lzma_rust2::LzmaReader::new_mem_limit(reader, DICTIONARY_LIMIT_KIB, None)
                .map(|decoder| Box::new(decoder) as Box<dyn Read + 'a>)
                .map_err(|error| VfsError::corrupt(format!("lzma header: {error}"))),
            Self::Zstd => Err(VfsError::unsupported(
                "zstd compressed content: this build has no zstd decoder",
            )),
        }
    }
}

/// Where one file's content lies.
#[derive(Debug, Clone, Default)]
pub(crate) struct Span {
    /// Index into [`SpanIndex::sources`].
    pub source: usize,
    /// Byte ranges, in order, as offset and length.
    pub extents: Vec<(u64, u64)>,
}

impl Span {
    /// One contiguous range.
    pub(crate) fn single(source: usize, offset: u64, len: u64) -> Self {
        Self {
            source,
            extents: vec![(offset, len)],
        }
    }

    fn len(&self) -> u64 {
        self.extents
            .iter()
            .fold(0u64, |total, (_, len)| total.saturating_add(*len))
    }
}

/// The sources a listing points into, and each file's ranges in them.
#[derive(Debug, Default)]
pub(crate) struct SpanIndex {
    /// Seekable sources: the container itself or a decoded payload.
    pub sources: Vec<ArchiveBacking>,
    /// One record per file, addressed by the listing's locator.
    pub spans: Vec<Span>,
}

impl SpanIndex {
    /// Record a span and return the locator the listing stores for it.
    pub(crate) fn push(&mut self, span: Span) -> u64 {
        self.spans.push(span);
        (self.spans.len() - 1) as u64
    }

    /// Expand the file recorded at `locator`.
    ///
    /// # Errors
    /// Returns [`VfsError::NotFound`] when the locator names no record,
    /// [`VfsError::Corrupt`] when a range runs past the end of its source, and
    /// [`VfsError::LimitExceeded`] when a ceiling trips.
    pub(crate) fn open_entry(
        &self,
        locator: Option<u64>,
        path: &VfsPath,
        limits: &Limits,
        budget: &Budget,
        cancel: &Cancel,
    ) -> VfsResult<OpenFile> {
        cancel.check()?;
        let span = locator
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.spans.get(index))
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
        let source = self
            .sources
            .get(span.source)
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
        let reader = ExtentReader::new(source.reader()?, span.extents.clone());
        let expected = span.len();
        let open = materialize(reader, expected, limits, budget, cancel)?;
        if open.len_hint() != Some(expected) {
            return Err(VfsError::corrupt(format!(
                "{} ends before the size its record states",
                path.as_str()
            )));
        }
        Ok(open)
    }
}

/// Reads a list of ranges of a seekable source as one stream.
struct ExtentReader<R> {
    inner: R,
    extents: std::vec::IntoIter<(u64, u64)>,
    left: u64,
}

impl<R: Read + Seek> ExtentReader<R> {
    fn new(inner: R, extents: Vec<(u64, u64)>) -> Self {
        Self {
            inner,
            extents: extents.into_iter(),
            left: 0,
        }
    }
}

impl<R: Read + Seek> Read for ExtentReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.left == 0 {
            let Some((offset, len)) = self.extents.next() else {
                return Ok(0);
            };
            self.inner.seek(SeekFrom::Start(offset))?;
            self.left = len;
        }
        let want = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let slice = buf.get_mut(..want).unwrap_or_default();
        let read = self.inner.read(slice)?;
        if read == 0 {
            // A short source ends the stream; the caller compares the length
            // against the record.
            self.left = 0;
            self.extents = Vec::new().into_iter();
            return Ok(0);
        }
        self.left = self.left.saturating_sub(read as u64);
        Ok(read)
    }
}

/// Decode `reader` into a temporary file under the ceilings.
///
/// # Errors
/// Returns [`VfsError::LimitExceeded`] when the stream expands past a ceiling,
/// [`VfsError::Cancelled`] when the flag is raised, and [`VfsError::Corrupt`]
/// when the decoder fails.
pub(crate) fn decode_to_temp<R: Read>(
    reader: R,
    what: &str,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
) -> VfsResult<ArchiveBacking> {
    let mut limited = LimitedReader::new(reader, 0, *limits, budget.clone(), cancel.clone());
    let mut temp = tempfile::NamedTempFile::new()?;
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        cancel.check()?;
        let read = match limited.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                let label = what.to_owned();
                return Err(uncarry_or(error, move |error| {
                    VfsError::corrupt(format!("{label}: {error}"))
                }));
            }
        };
        temp.write_all(chunk.get(..read).unwrap_or_default())?;
    }
    temp.flush()?;
    Ok(ArchiveBacking::Temp(Arc::new(temp)))
}

/// A reader over `len` bytes of `backing` starting at `offset`.
///
/// # Errors
/// Returns [`VfsError::Io`] when the backing cannot be opened or sought.
pub(crate) fn range_reader(
    backing: &ArchiveBacking,
    offset: u64,
    len: u64,
) -> VfsResult<io::Take<super::BackingReader>> {
    let mut reader = backing.reader()?;
    reader.seek(SeekFrom::Start(offset))?;
    Ok(reader.take(len))
}

/// The length of a backing, found by seeking to its end.
///
/// # Errors
/// Returns [`VfsError::Io`] when the backing cannot be opened or sought.
pub(crate) fn backing_len(backing: &ArchiveBacking) -> VfsResult<u64> {
    let mut reader = backing.reader()?;
    Ok(reader.seek(SeekFrom::End(0))?)
}

/// Fill `buf` from `reader`, failing as corrupt when the source ends first.
///
/// # Errors
/// Returns [`VfsError::Corrupt`] on a short source and [`VfsError::Io`] on any
/// other read failure.
pub(crate) fn read_exact_or_corrupt<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    what: &str,
) -> VfsResult<()> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(VfsError::corrupt(format!("{what}: truncated")))
        }
        Err(error) => Err(crate::limits::uncarry(error)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn read_all(index: &SpanIndex, locator: u64) -> VfsResult<Vec<u8>> {
        let path = VfsPath::parse("a").unwrap();
        let mut open = index.open_entry(
            Some(locator),
            &path,
            &Limits::default(),
            &Budget::new(u64::MAX),
            &Cancel::new(),
        )?;
        let mut out = Vec::new();
        open.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn extents_read_as_one_stream() {
        let mut index = SpanIndex::default();
        index
            .sources
            .push(ArchiveBacking::from_bytes(b"0123456789".to_vec()));
        let locator = index.push(Span {
            source: 0,
            extents: vec![(2, 3), (7, 2)],
        });
        assert_eq!(read_all(&index, locator).unwrap(), b"23478");
    }

    #[test]
    fn a_range_past_the_end_is_corrupt() {
        let mut index = SpanIndex::default();
        index
            .sources
            .push(ArchiveBacking::from_bytes(b"0123".to_vec()));
        let locator = index.push(Span::single(0, 2, 100));
        assert!(matches!(
            read_all(&index, locator),
            Err(VfsError::Corrupt { .. })
        ));
    }
}
