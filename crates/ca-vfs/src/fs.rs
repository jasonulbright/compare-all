//! The trait every browsable source implements, and the handle it hands back.

use std::io::{self, Read, Seek, SeekFrom};

use crate::cancel::Cancel;
use crate::entry::VfsEntry;
use crate::error::{VfsError, VfsResult};
use crate::path::VfsPath;

/// What one file system can do.
///
/// A caller reads these flags instead of probing with an operation that would
/// fail, so a folder view can grey out commands before they are attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a record of independent capability flags"
)]
pub struct Capabilities {
    /// Write, delete and rename are implemented.
    pub writable: bool,
    /// Listings carry modification times, each at the precision its
    /// [`crate::VfsEntry::time_fidelity`] states.
    ///
    /// No comparison reads this flag. An entry whose
    /// [`crate::VfsEntry::modified`] is `None` is what keeps the timestamp
    /// test from settling a pair.
    pub supports_timestamps: bool,
    /// Listings carry attributes.
    pub supports_attributes: bool,
    /// Listings may carry a CRC32 taken from the source's own metadata.
    ///
    /// The value is what the container or the capture recorded, not something
    /// this crate checked against the bytes. It is evidence in one direction
    /// only: two entries whose stored checksums differ, or whose exact sizes
    /// differ, are certainly not the same content, while two that agree may
    /// still differ and have to be read to be ruled equal. An entry that
    /// carries no checksum leaves [`crate::VfsEntry::crc32`] empty rather than
    /// reporting a zero, so a missing checksum is never mistaken for a value
    /// two entries happen to share.
    ///
    /// A checksum that is present is verified when the entry is read in full,
    /// and a disagreement is reported as
    /// [`VfsError::ChecksumMismatch`](crate::VfsError::ChecksumMismatch).
    pub stored_crc: bool,
    /// Opened files support seeking.
    pub random_access: bool,
    /// Files can be opened at all. A listing-only source reports false and
    /// fails every open with [`VfsError::ContentNotStored`].
    pub content_available: bool,
}

impl Capabilities {
    /// A read-only source with no metadata beyond names and sizes.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            writable: false,
            supports_timestamps: false,
            supports_attributes: false,
            stored_crc: false,
            random_access: false,
            content_available: true,
        }
    }
}

/// A readable source that may also be seekable.
///
/// Implemented by the two adapters in this module; a file system returns one
/// of them from [`FileSystem::open`].
pub trait ReadSource: Read + Send {
    /// Seek when the underlying source allows it, otherwise `None`.
    fn seek_to(&mut self, pos: SeekFrom) -> Option<io::Result<u64>>;
}

/// Wraps a stream that cannot seek.
#[derive(Debug)]
pub struct StreamSource<R>(R);

impl<R: Read + Send> StreamSource<R> {
    /// Wrap `inner`.
    pub const fn new(inner: R) -> Self {
        Self(inner)
    }
}

impl<R: Read + Send> Read for StreamSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<R: Read + Send> ReadSource for StreamSource<R> {
    fn seek_to(&mut self, _pos: SeekFrom) -> Option<io::Result<u64>> {
        None
    }
}

/// Wraps a source that can seek.
#[derive(Debug)]
pub struct SeekableSource<R>(R);

impl<R: Read + Seek + Send> SeekableSource<R> {
    /// Wrap `inner`.
    pub const fn new(inner: R) -> Self {
        Self(inner)
    }
}

impl<R: Read + Seek + Send> Read for SeekableSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<R: Read + Seek + Send> ReadSource for SeekableSource<R> {
    fn seek_to(&mut self, pos: SeekFrom) -> Option<io::Result<u64>> {
        Some(self.0.seek(pos))
    }
}

/// An open file handle.
///
/// The handle always reads; whether it seeks is a property of the source that
/// produced it, so callers query [`OpenFile::is_seekable`] rather than
/// downcasting.
pub struct OpenFile {
    source: Box<dyn ReadSource>,
    len: Option<u64>,
    seekable: bool,
}

impl std::fmt::Debug for OpenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenFile")
            .field("len", &self.len)
            .field("seekable", &self.seekable)
            .finish_non_exhaustive()
    }
}

impl OpenFile {
    /// Build a handle over a stream that cannot seek.
    #[must_use]
    pub fn streaming<R: Read + Send + 'static>(inner: R, len: Option<u64>) -> Self {
        Self {
            source: Box::new(StreamSource::new(inner)),
            len,
            seekable: false,
        }
    }

    /// Build a handle over a source that can seek.
    #[must_use]
    pub fn seekable<R: Read + Seek + Send + 'static>(inner: R, len: Option<u64>) -> Self {
        Self {
            source: Box::new(SeekableSource::new(inner)),
            len,
            seekable: true,
        }
    }

    /// Whether [`OpenFile::seek`] works on this handle.
    #[must_use]
    pub fn is_seekable(&self) -> bool {
        self.seekable
    }

    /// Content length where the source knows it in advance.
    #[must_use]
    pub fn len_hint(&self) -> Option<u64> {
        self.len
    }

    /// Move the read position.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] when the handle does not seek, or
    /// [`VfsError::Io`] when the seek itself fails.
    pub fn seek(&mut self, pos: SeekFrom) -> VfsResult<u64> {
        match self.source.seek_to(pos) {
            Some(result) => Ok(result?),
            None => Err(VfsError::unsupported("seek on a streaming source")),
        }
    }
}

impl Read for OpenFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.source.read(buf)
    }
}

/// A browsable source: a local directory, a container read as folders, or a
/// recorded listing.
///
/// Implementations are shared across worker threads, so every method takes
/// `&self` and every long running method takes a [`Cancel`] it polls often
/// enough that a cancelled scan stops promptly.
pub trait FileSystem: Send + Sync {
    /// What this source can do.
    fn capabilities(&self) -> Capabilities;

    /// A label for the root, such as the archive file name or origin folder.
    fn root_label(&self) -> String;

    /// Entries directly under `dir`, in no guaranteed order.
    ///
    /// # Errors
    /// Returns [`VfsError::NotFound`] or [`VfsError::NotADirectory`] when
    /// `dir` does not name a directory, and [`VfsError::Cancelled`] when the
    /// flag is raised part way through.
    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>>;

    /// The listing record for one path.
    ///
    /// # Errors
    /// Returns [`VfsError::NotFound`] when nothing is stored at `path`.
    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry>;

    /// Open a file for reading.
    ///
    /// # Errors
    /// Returns [`VfsError::IsADirectory`], [`VfsError::ContentNotStored`] for
    /// listing-only sources, [`VfsError::Refused`] for an entry the listing
    /// marks refused, [`VfsError::NeedsPassword`] for encrypted entries, and
    /// [`VfsError::LimitExceeded`] when expanding the entry would pass a
    /// configured ceiling.
    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile>;

    /// Create a directory, and any missing parent of it.
    ///
    /// # Errors
    /// Returns [`VfsError::ReadOnly`] when the source does not accept writes.
    fn create_dir(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let _ = (path, cancel);
        Err(VfsError::ReadOnly)
    }

    /// Create or replace a file with everything `content` yields.
    ///
    /// # Errors
    /// Returns [`VfsError::ReadOnly`] when the source does not accept writes.
    fn write_file(&self, path: &VfsPath, content: &mut dyn Read, cancel: &Cancel) -> VfsResult<()> {
        let _ = (path, content, cancel);
        Err(VfsError::ReadOnly)
    }

    /// Delete a file, or a directory and everything under it.
    ///
    /// # Errors
    /// Returns [`VfsError::ReadOnly`] when the source does not accept writes.
    fn delete(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let _ = (path, cancel);
        Err(VfsError::ReadOnly)
    }

    /// Move an entry, and everything under it, to a new path.
    ///
    /// # Errors
    /// Returns [`VfsError::ReadOnly`] when the source does not accept writes.
    fn rename(&self, from: &VfsPath, to: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let _ = (from, to, cancel);
        Err(VfsError::ReadOnly)
    }

    /// An identity for what a directory link at `path` points at.
    ///
    /// Two links that resolve to the same place return the same string. A
    /// source that has no links, or cannot resolve one, returns `None`, which
    /// a walk treats as a link it must not descend through.
    fn link_identity(&self, path: &VfsPath) -> Option<String> {
        let _ = path;
        None
    }
}

/// How deep a walk descends before it stops and records why.
///
/// A container can name an entry far deeper than any real tree, and a link
/// loop can manufacture depth without end, so the walk is bounded whether or
/// not the cycle guard catches the cause.
pub const MAX_WALK_DEPTH: usize = 256;

/// Walk the whole tree under `dir`, depth first.
///
/// A directory link whose target already appears on the branch being walked is
/// listed but not descended into, because following it would hand the same
/// files back for as long as the path can grow. A branch is also cut at
/// [`MAX_WALK_DEPTH`]. Either way the entry is kept and carries the reason, so
/// the walk reports less of the tree but never silently loops.
///
/// # Errors
/// Propagates whatever [`FileSystem::list`] reports for the root of the walk;
/// a directory below it that fails to list is recorded on the entry's `error`
/// field instead of ending the walk.
pub fn walk(fs: &dyn FileSystem, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
    /// One directory still to visit, with the link targets already crossed to
    /// reach it.
    struct Pending {
        path: VfsPath,
        branch: std::sync::Arc<Vec<String>>,
    }

    let mut out: Vec<VfsEntry> = Vec::new();
    let mut stack = vec![Pending {
        path: dir.clone(),
        branch: std::sync::Arc::new(Vec::new()),
    }];
    let mut first = true;

    while let Some(next) = stack.pop() {
        cancel.check()?;
        let listed = match fs.list(&next.path, cancel) {
            Ok(entries) => entries,
            Err(VfsError::Cancelled) => return Err(VfsError::Cancelled),
            Err(error) if first => return Err(error),
            Err(error) => {
                if let Some(entry) = out.iter_mut().find(|e: &&mut VfsEntry| e.path == next.path) {
                    // A refused directory lists under a mapped name that a
                    // local folder cannot list, and its error already states
                    // why its content is out of reach.
                    if !entry.refused {
                        entry.error = Some(error.to_string());
                    }
                }
                continue;
            }
        };
        first = false;

        for mut entry in listed {
            if entry.is_dir() {
                if entry.path.depth() >= MAX_WALK_DEPTH {
                    entry.error = Some(format!(
                        "not descended into: the tree is more than {MAX_WALK_DEPTH} levels deep"
                    ));
                } else if entry.is_link() {
                    match fs.link_identity(&entry.path) {
                        Some(target) if next.branch.contains(&target) => {
                            entry.error = Some(
                                "not descended into: the link points back into the branch that \
                                 reaches it"
                                    .to_owned(),
                            );
                        }
                        Some(target) => {
                            let mut branch = (*next.branch).clone();
                            branch.push(target);
                            stack.push(Pending {
                                path: entry.path.clone(),
                                branch: std::sync::Arc::new(branch),
                            });
                        }
                        None => {
                            entry.error = Some(
                                "not descended into: the link target could not be identified"
                                    .to_owned(),
                            );
                        }
                    }
                } else {
                    stack.push(Pending {
                        path: entry.path.clone(),
                        branch: std::sync::Arc::clone(&next.branch),
                    });
                }
            }
            out.push(entry);
        }
    }
    Ok(out)
}
