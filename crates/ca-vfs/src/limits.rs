//! Ceilings that bound what a crafted container can cost to expand.
//!
//! A container declares how large its entries are, and that declaration is not
//! evidence. Every byte handed to a caller passes through [`LimitedReader`],
//! which counts what actually came out and fails before the cost of a
//! deliberately over-compressed entry lands on the caller.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::cancel::Cancel;
use crate::error::{LimitKind, VfsError, VfsResult};
use crate::fs::OpenFile;

/// Ceilings applied while expanding a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most bytes one entry may expand to.
    pub max_entry_bytes: u64,
    /// Most bytes one container may expand to across every entry read from it.
    pub max_archive_bytes: u64,
    /// Most times larger than its stored size one entry may expand.
    pub max_expansion_ratio: u64,
    /// Expansion below this size is always allowed, whatever the ratio. A tiny
    /// stored size otherwise makes the ratio ceiling unusably tight.
    pub ratio_floor_bytes: u64,
    /// How many containers may be opened through one another.
    pub max_nesting_depth: usize,
    /// Expanded content up to this size stays in memory; past it the rest
    /// spills to a temporary file.
    pub memory_spill_bytes: u64,
    /// Most seconds a helper process that reads a container may run.
    pub max_helper_seconds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entry_bytes: 2 * GIB,
            max_archive_bytes: 8 * GIB,
            max_expansion_ratio: 500,
            ratio_floor_bytes: 16 * MIB,
            max_nesting_depth: 4,
            memory_spill_bytes: 16 * MIB,
            max_helper_seconds: 60 * 60,
        }
    }
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

impl Limits {
    /// The ceiling for one entry, given the size it occupies in the container.
    ///
    /// `stored` of zero means the container did not say, in which case only
    /// the absolute per-entry ceiling applies.
    #[must_use]
    pub fn entry_ceiling(&self, stored: u64) -> u64 {
        if stored == 0 {
            return self.max_entry_bytes;
        }
        let by_ratio = stored
            .saturating_mul(self.max_expansion_ratio)
            .max(self.ratio_floor_bytes);
        by_ratio.min(self.max_entry_bytes)
    }

    /// Whether `stored` expanding to `produced` breaks the ratio ceiling.
    #[must_use]
    pub fn ratio_exceeded(&self, stored: u64, produced: u64) -> bool {
        if stored == 0 || produced <= self.ratio_floor_bytes {
            return false;
        }
        produced / stored.max(1) > self.max_expansion_ratio
    }
}

/// Bytes still available to one container across all of its entries.
#[derive(Debug, Clone)]
pub struct Budget {
    remaining: Arc<AtomicU64>,
    total: u64,
}

impl Budget {
    /// A budget of `total` bytes.
    #[must_use]
    pub fn new(total: u64) -> Self {
        Self {
            remaining: Arc::new(AtomicU64::new(total)),
            total,
        }
    }

    /// Take `bytes` from the budget, failing once it is empty.
    ///
    /// # Errors
    /// Returns [`VfsError::LimitExceeded`] when the container has expanded
    /// past [`Limits::max_archive_bytes`].
    pub fn take(&self, bytes: u64) -> VfsResult<()> {
        let mut current = self.remaining.load(Ordering::SeqCst);
        loop {
            if current < bytes {
                return Err(VfsError::LimitExceeded {
                    kind: LimitKind::ArchiveSize,
                    limit: self.total,
                });
            }
            match self.remaining.compare_exchange_weak(
                current,
                current - bytes,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(()),
                Err(seen) => current = seen,
            }
        }
    }
}

/// The reasons this crate stops a decoder from inside a [`Read`].
///
/// The payload is cloneable so it can be rebuilt from a shared reference, which
/// is all a codec crate leaves once it has wrapped the error in one of its own.
#[derive(Debug, Clone)]
enum Carried {
    Cancelled,
    Timeout {
        operation: Arc<str>,
    },
    Limit {
        kind: LimitKind,
        limit: u64,
    },
    ResourceLimit {
        resource: Arc<str>,
    },
    Checksum {
        path: Arc<str>,
        expected: u32,
        actual: u32,
    },
}

impl Carried {
    fn rebuild(self) -> VfsError {
        match self {
            Self::Cancelled => VfsError::Cancelled,
            Self::Timeout { operation } => VfsError::Timeout {
                operation: operation.to_string(),
            },
            Self::Limit { kind, limit } => VfsError::LimitExceeded { kind, limit },
            Self::ResourceLimit { resource } => VfsError::ResourceLimit {
                resource: resource.to_string(),
            },
            Self::Checksum {
                path,
                expected,
                actual,
            } => VfsError::ChecksumMismatch {
                path: crate::path::VfsPath::parse(&path)
                    .unwrap_or_else(|_| crate::path::VfsPath::root()),
                expected,
                actual,
            },
        }
    }
}

/// An error carried out through [`std::io::Error`] and recovered afterwards.
#[derive(Debug)]
struct CarriedError(String, Carried);

impl std::fmt::Display for CarriedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CarriedError {}

/// Wrap a crate error so it can travel through a [`Read`] implementation.
pub(crate) fn carry(error: VfsError) -> io::Error {
    let text = error.to_string();
    let payload = match error {
        VfsError::Cancelled => Carried::Cancelled,
        VfsError::Timeout { ref operation } => Carried::Timeout {
            operation: Arc::from(operation.as_str()),
        },
        VfsError::LimitExceeded { kind, limit } => Carried::Limit { kind, limit },
        VfsError::ResourceLimit { ref resource } => Carried::ResourceLimit {
            resource: Arc::from(resource.as_str()),
        },
        VfsError::ChecksumMismatch {
            path,
            expected,
            actual,
        } => Carried::Checksum {
            path: Arc::from(path.as_str()),
            expected,
            actual,
        },
        VfsError::Io(io) => return io,
        other => return io::Error::other(other.to_string()),
    };
    io::Error::other(CarriedError(text, payload))
}

/// Recover an error put into an [`std::io::Error`] by [`carry`].
///
/// Codec crates wrap a reader's failure in an error of their own, sometimes
/// several layers deep, so the whole source chain is searched rather than the
/// outermost error alone.
pub(crate) fn uncarry(error: io::Error) -> VfsError {
    if let Some(found) = find_carried(&error) {
        return found.rebuild();
    }
    VfsError::Io(error)
}

/// The carried payload anywhere in `error`'s source chain.
fn find_carried(error: &(dyn std::error::Error + 'static)) -> Option<Carried> {
    let mut current = Some(error);
    while let Some(node) = current {
        if let Some(carried) = node.downcast_ref::<CarriedError>() {
            return Some(carried.1.clone());
        }
        if let Some(io) = node.downcast_ref::<io::Error>() {
            if let Some(inner) = io.get_ref() {
                if let Some(carried) = find_carried(inner) {
                    return Some(carried);
                }
            }
        }
        current = node.source();
    }
    None
}

/// Recover a carried error from anything that wraps one, or build a fallback.
pub(crate) fn uncarry_or(
    error: io::Error,
    fallback: impl FnOnce(io::Error) -> VfsError,
) -> VfsError {
    if let Some(found) = find_carried(&error) {
        return found.rebuild();
    }
    fallback(error)
}

/// Recover a carried error out of any error type that may wrap one.
pub(crate) fn uncarried<E>(error: &E) -> Option<VfsError>
where
    E: std::error::Error + 'static,
{
    find_carried(error).map(Carried::rebuild)
}

/// A reader that stops on cancellation and on any expansion ceiling.
pub struct LimitedReader<R> {
    inner: R,
    produced: u64,
    stored: u64,
    ceiling: u64,
    limits: Limits,
    budget: Budget,
    cancel: Cancel,
}

impl<R: Read> LimitedReader<R> {
    /// Wrap `inner`, which expands from `stored` bytes in the container.
    pub fn new(inner: R, stored: u64, limits: Limits, budget: Budget, cancel: Cancel) -> Self {
        let ceiling = limits.entry_ceiling(stored);
        Self {
            inner,
            produced: 0,
            stored,
            ceiling,
            limits,
            budget,
            cancel,
        }
    }

    /// Bytes handed out so far.
    #[must_use]
    pub fn produced(&self) -> u64 {
        self.produced
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(carry(VfsError::Cancelled));
        }
        let read = self.inner.read(buf)?;
        if read == 0 {
            return Ok(0);
        }
        let read64 = read as u64;
        self.produced = self.produced.saturating_add(read64);
        if self.produced > self.ceiling {
            return Err(carry(VfsError::LimitExceeded {
                kind: LimitKind::EntrySize,
                limit: self.ceiling,
            }));
        }
        if self.limits.ratio_exceeded(self.stored, self.produced) {
            return Err(carry(VfsError::LimitExceeded {
                kind: LimitKind::ExpansionRatio,
                limit: self.limits.max_expansion_ratio,
            }));
        }
        if let Err(error) = self.budget.take(read64) {
            return Err(carry(error));
        }
        Ok(read)
    }
}

/// Expanded content, held in memory while it is small and spilled to a
/// temporary file once it is not.
#[derive(Debug)]
pub enum Spill {
    /// Content small enough to keep in memory.
    Memory(io::Cursor<Vec<u8>>),
    /// Content written to a temporary file that is deleted when dropped.
    File(std::fs::File),
}

impl Read for Spill {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Memory(cursor) => cursor.read(buf),
            Self::File(file) => file.read(buf),
        }
    }
}

impl Seek for Spill {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Memory(cursor) => cursor.seek(pos),
            Self::File(file) => file.seek(pos),
        }
    }
}

/// Read everything `reader` yields under the given ceilings, spilling to a
/// temporary file past [`Limits::memory_spill_bytes`], and hand back a
/// seekable handle positioned at the start.
///
/// Expanding the whole entry up front is what makes a container entry look
/// like an ordinary file: the decoders behind these formats borrow the
/// container while they run, so the handle cannot hold one open.
///
/// # Errors
/// Returns [`VfsError::LimitExceeded`] when a ceiling trips, [`VfsError::Cancelled`]
/// when the flag is raised, and [`VfsError::Io`] for anything else.
pub fn materialize<R: Read>(
    reader: R,
    stored: u64,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
) -> VfsResult<OpenFile> {
    let mut limited = LimitedReader::new(reader, stored, *limits, budget.clone(), cancel.clone());
    let mut memory: Vec<u8> = Vec::new();
    let mut spilled: Option<std::fs::File> = None;
    let mut chunk = vec![0u8; 64 * 1024];

    loop {
        let read = match limited.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => return Err(uncarry(error)),
        };
        let slice = chunk.get(..read).unwrap_or_default();
        if let Some(file) = spilled.as_mut() {
            file.write_all(slice)?;
            continue;
        }
        memory.extend_from_slice(slice);
        if memory.len() as u64 > limits.memory_spill_bytes {
            let mut file = tempfile::tempfile()?;
            file.write_all(&memory)?;
            memory = Vec::new();
            spilled = Some(file);
        }
    }

    if let Some(mut file) = spilled {
        file.flush()?;
        let len = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        return Ok(OpenFile::seekable(Spill::File(file), Some(len)));
    }
    let len = memory.len() as u64;
    Ok(OpenFile::seekable(
        Spill::Memory(io::Cursor::new(memory)),
        Some(len),
    ))
}

/// Read everything `reader` yields into memory under the given ceilings.
///
/// # Errors
/// Same conditions as [`materialize`].
pub(crate) fn read_bounded<R: Read>(
    reader: R,
    stored: u64,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
) -> VfsResult<Vec<u8>> {
    let mut limited = LimitedReader::new(reader, stored, *limits, budget.clone(), cancel.clone());
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match limited.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => out.extend_from_slice(chunk.get(..read).unwrap_or_default()),
            Err(error) => return Err(uncarry(error)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn entry_ceiling_honours_ratio_and_absolute_cap() {
        let limits = Limits {
            max_entry_bytes: 1000,
            ratio_floor_bytes: 10,
            max_expansion_ratio: 10,
            ..Limits::default()
        };
        assert_eq!(limits.entry_ceiling(0), 1000);
        assert_eq!(limits.entry_ceiling(5), 50);
        assert_eq!(limits.entry_ceiling(1_000_000), 1000);
    }

    #[test]
    fn limited_reader_trips_on_entry_size() {
        let limits = Limits {
            max_entry_bytes: 8,
            ratio_floor_bytes: 0,
            ..Limits::default()
        };
        let data = vec![0u8; 64];
        let error = read_bounded(
            data.as_slice(),
            0,
            &limits,
            &Budget::new(u64::MAX),
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            VfsError::LimitExceeded {
                kind: LimitKind::EntrySize,
                ..
            }
        ));
    }

    #[test]
    fn budget_trips_across_entries() {
        let budget = Budget::new(10);
        budget.take(8).unwrap();
        assert!(matches!(
            budget.take(8),
            Err(VfsError::LimitExceeded {
                kind: LimitKind::ArchiveSize,
                ..
            })
        ));
    }

    #[test]
    fn cancellation_stops_a_read() {
        let cancel = Cancel::new();
        cancel.cancel();
        let data = vec![0u8; 64];
        let error = read_bounded(
            data.as_slice(),
            0,
            &Limits::default(),
            &Budget::new(u64::MAX),
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(error, VfsError::Cancelled));
    }

    #[test]
    fn remote_resource_limit_survives_io_wrapping() {
        let original = VfsError::ResourceLimit {
            resource: "SFTP response packet".to_owned(),
        };
        let recovered = uncarry(carry(original));

        assert!(matches!(
            recovered,
            VfsError::ResourceLimit { ref resource } if resource == "SFTP response packet"
        ));
    }

    #[test]
    fn spill_round_trips_past_the_memory_threshold() {
        let limits = Limits {
            memory_spill_bytes: 16,
            ..Limits::default()
        };
        let data = vec![7u8; 1024];
        let mut open = materialize(
            data.as_slice(),
            1024,
            &limits,
            &Budget::new(u64::MAX),
            &Cancel::new(),
        )
        .unwrap();
        assert!(open.is_seekable());
        let mut out = Vec::new();
        open.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }
}
