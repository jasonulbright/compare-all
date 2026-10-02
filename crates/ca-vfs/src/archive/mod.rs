//! Containers browsed as folders.
//!
//! One [`ArchiveFs`] covers every supported format. The listing is built once
//! when the container is opened and cached, because every backing format costs
//! a full pass over the container to enumerate. Opening an entry expands it
//! under the ceilings in [`crate::Limits`]; see [`crate::limits::materialize`]
//! for why an entry is expanded up front rather than streamed straight out of
//! the decoder.

pub(crate) mod cab_format;
pub(crate) mod cpio;
pub(crate) mod deb_format;
pub(crate) mod iso_format;
pub(crate) mod rar_format;
pub(crate) mod rpm_format;
pub(crate) mod sevenz_format;
pub(crate) mod single;
pub(crate) mod span;
pub(crate) mod tar_format;
mod zip_copy;
pub(crate) mod zip_format;

use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::cancel::Cancel;
use crate::detect::{detect, ArchiveFormat, ArchiveTypes};
use crate::entry::{EntryKind, VfsEntry};
use crate::error::{LimitKind, VfsError, VfsResult};
use crate::fs::{Capabilities, FileSystem, OpenFile};
use crate::limits::{Budget, Limits};
use crate::path::VfsPath;
use crate::tree::Tree;

/// Bytes shared between readers over the same in-memory container.
#[derive(Debug, Clone)]
pub struct SharedBytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Where a container's bytes live.
#[derive(Debug, Clone)]
pub enum ArchiveBacking {
    /// A file on disk.
    Path(PathBuf),
    /// Bytes held in memory, used for a small container found inside another.
    Memory(SharedBytes),
    /// A temporary file, used for a large container found inside another. The
    /// file is removed when the last handle to it drops.
    Temp(Arc<tempfile::NamedTempFile>),
}

/// A reader over a container, whatever it is backed by.
#[derive(Debug)]
pub enum BackingReader {
    /// Reader over a file on disk.
    File(File),
    /// Reader over bytes in memory.
    Memory(Cursor<SharedBytes>),
}

impl Read for BackingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::File(file) => file.read(buf),
            Self::Memory(cursor) => cursor.read(buf),
        }
    }
}

impl Seek for BackingReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::File(file) => file.seek(pos),
            Self::Memory(cursor) => cursor.seek(pos),
        }
    }
}

impl ArchiveBacking {
    /// Bytes of an in-memory container.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self::Memory(SharedBytes(Arc::new(bytes)))
    }

    /// A fresh reader positioned at the start.
    ///
    /// # Errors
    /// Returns [`VfsError::Io`] when the backing file cannot be opened.
    pub fn reader(&self) -> VfsResult<BackingReader> {
        match self {
            Self::Path(path) => Ok(BackingReader::File(File::open(path)?)),
            Self::Memory(bytes) => Ok(BackingReader::Memory(Cursor::new(bytes.clone()))),
            Self::Temp(temp) => Ok(BackingReader::File(File::open(temp.path())?)),
        }
    }

    /// The file this container lives in, when it lives in one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path.as_path()),
            Self::Temp(temp) => Some(temp.path()),
            Self::Memory(_) => None,
        }
    }
}

/// How a container should be opened.
#[derive(Debug, Clone, Default)]
pub struct ArchiveOptions {
    /// Expansion ceilings.
    pub limits: Limits,
    /// Which extensions name which format.
    pub types: ArchiveTypes,
    /// Password for encrypted entries.
    pub password: Option<String>,
    /// How many containers this one is already inside.
    pub depth: usize,
    /// Seconds the wall clock of a DOS stamp runs ahead of UTC.
    ///
    /// A zip or cabinet entry that carries only a DOS stamp is read as the
    /// instant that wall clock names in this zone, and a zip record this build
    /// writes carries its DOS stamp in this zone. Zero reads the stamp as UTC.
    pub zone_offset_seconds: i32,
}

/// Which backend reads the container.
#[derive(Debug)]
enum Backend {
    /// Zip and the formats that are zip containers under another name.
    Zip,
    /// Tar, optionally inside one compressed stream.
    Tar(Option<ArchiveFormat>),
    /// A single file inside one compressed stream.
    Single(single::SingleStream),
    /// 7z.
    SevenZip,
    /// Formats whose entries are byte ranges of the container or of a payload
    /// decoded once: disc images and both package formats.
    Spans(span::SpanIndex),
    /// Microsoft cabinet.
    Cab(cab_format::CabIndex),
    /// RAR, converted to a plain tar held in a temporary file.
    Rar(ArchiveBacking),
}

/// A container presented as a read-only, or for zip a writable, file system.
pub struct ArchiveFs {
    backing: ArchiveBacking,
    format: ArchiveFormat,
    label: String,
    backend: RwLock<Backend>,
    tree: RwLock<Tree>,
    options: ArchiveOptions,
    /// The whole tar, decompressed once, so a later open seeks instead of
    /// decoding the stream again from its first byte.
    tar_source: Mutex<Option<ArchiveBacking>>,
    /// Entries already decoded out of a solid 7z block.
    sevenz_cache: Mutex<sevenz_format::BlockCache>,
    /// What the container looked like on disk when it was opened, so a write
    /// cannot overwrite a version someone else replaced in the meantime.
    stamp: Mutex<Option<zip_format::Stamp>>,
    write_lock: Mutex<()>,
    rewrites: std::sync::atomic::AtomicU64,
}

/// One change in a batch applied to a writable container.
#[derive(Debug, Clone)]
pub enum ArchiveEdit {
    /// Add a directory entry.
    CreateDir {
        /// Directory to add.
        path: VfsPath,
    },
    /// Create or replace a file.
    WriteFile {
        /// File to write.
        path: VfsPath,
        /// Bytes to store.
        content: Vec<u8>,
        /// The modification time the record carries. `None` stamps it with
        /// the time of the rewrite.
        modified: Option<std::time::SystemTime>,
    },
    /// Remove an entry and everything under it.
    Delete {
        /// Entry to remove.
        path: VfsPath,
    },
    /// Move an entry and everything under it.
    Rename {
        /// Entry to move.
        from: VfsPath,
        /// New path.
        to: VfsPath,
    },
}

impl ArchiveEdit {
    fn to_mutation(&self, tree: &Tree) -> VfsResult<zip_format::Mutation> {
        Ok(match self {
            Self::CreateDir { path } => added_directory(tree, path)?,
            Self::WriteFile {
                path,
                content,
                modified,
            } => zip_format::Mutation::WriteFile {
                path: path.clone(),
                replaces: written(tree, path)?,
                content: content.clone(),
                modified: *modified,
            },
            Self::Delete { path } => zip_format::Mutation::Delete {
                entry: selected(tree, path),
            },
            Self::Rename { from, to } => zip_format::Mutation::Rename {
                from: selected(tree, from),
                to: to.clone(),
                replaces: renamed_over(tree, from, to)?,
            },
        })
    }
}

/// The record a rename of the listed row `from` onto the listed row `to`
/// replaces, or `None` for a name the listing does not hold.
///
/// A file row names one record, which may be stored under another name when
/// the listing moved it aside for a directory. The moved record takes that
/// stored name, so the row keeps its name and no second record appears.
///
/// # Errors
/// Returns [`VfsError::AlreadyExists`] when `to` is a directory row, when
/// `from` is a directory row, and for a file row with no record behind it.
fn renamed_over(tree: &Tree, from: &VfsPath, to: &VfsPath) -> VfsResult<Option<usize>> {
    if from == to {
        return Ok(None);
    }
    let Some(target) = tree.get(to) else {
        return Ok(None);
    };
    let already = || VfsError::AlreadyExists { path: to.clone() };
    if target.is_dir() || tree.get(from).is_some_and(VfsEntry::is_dir) {
        return Err(already());
    }
    tree.locator(to)
        .and_then(|locator| usize::try_from(locator).ok())
        .map(Some)
        .ok_or_else(already)
}

/// The record a write to the listed row `path` replaces, or `None` for a
/// name the listing does not hold.
///
/// A file row names one record, which keeps its position and its stored
/// name. For a file the listing moved aside for a directory, the stored name
/// is the directory's name, so a write under the listed name would add a
/// second record and move the row the write was meant for.
///
/// # Errors
/// Returns [`VfsError::IsADirectory`] for a directory row.
fn written(tree: &Tree, path: &VfsPath) -> VfsResult<Option<usize>> {
    match tree.get(path) {
        Some(entry) if entry.is_dir() => Err(VfsError::IsADirectory { path: path.clone() }),
        Some(_) => Ok(tree
            .locator(path)
            .and_then(|locator| usize::try_from(locator).ok())),
        None => Ok(None),
    }
}

/// The change that adds a directory at the listed path `path`.
///
/// # Errors
/// Returns [`VfsError::AlreadyExists`] when the listing shows a file there.
fn added_directory(tree: &Tree, path: &VfsPath) -> VfsResult<zip_format::Mutation> {
    if tree.get(path).is_some_and(|entry| !entry.is_dir()) {
        return Err(VfsError::AlreadyExists { path: path.clone() });
    }
    Ok(zip_format::Mutation::AddDirectory { path: path.clone() })
}

/// The stored records an edit of the listed row `path` acts on.
///
/// A file row names one record, which may be stored under another name when
/// the listing moved it aside for a directory. A directory row names the
/// directory's records and never a file record stored under its name.
fn selected(tree: &Tree, path: &VfsPath) -> zip_format::Selected {
    match tree.get(path) {
        Some(entry) if entry.is_dir() => zip_format::Selected::Directory(path.clone()),
        Some(_) => tree
            .locator(path)
            .and_then(|locator| usize::try_from(locator).ok())
            .map_or_else(
                || zip_format::Selected::Stored(path.clone()),
                zip_format::Selected::Record,
            ),
        None => zip_format::Selected::Stored(path.clone()),
    }
}

impl std::fmt::Debug for ArchiveFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveFs")
            .field("format", &self.format)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl ArchiveFs {
    /// Open the container at `path`.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] when the bytes name no format this
    /// build reads, and [`VfsError::Corrupt`] when the container does not
    /// parse.
    pub fn open_path(path: impl AsRef<Path>, options: ArchiveOptions) -> VfsResult<Self> {
        let path = path.as_ref();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::open(ArchiveBacking::Path(path.to_path_buf()), &name, options)
    }

    /// Open a container over `backing`, using `name` only where the bytes are
    /// ambiguous.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] when the bytes name no format this
    /// build reads, and [`VfsError::Corrupt`] when the container does not
    /// parse.
    pub fn open(backing: ArchiveBacking, name: &str, options: ArchiveOptions) -> VfsResult<Self> {
        Self::open_cancellable(backing, name, options, &Cancel::new())
    }

    /// [`ArchiveFs::open`], stopped with [`VfsError::Cancelled`] when `cancel`
    /// is raised while the listing is read.
    ///
    /// # Errors
    /// As [`ArchiveFs::open`], and [`VfsError::Cancelled`].
    #[allow(clippy::too_many_lines)]
    pub fn open_cancellable(
        backing: ArchiveBacking,
        name: &str,
        options: ArchiveOptions,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        if options.depth > options.limits.max_nesting_depth {
            return Err(VfsError::LimitExceeded {
                kind: LimitKind::NestingDepth,
                limit: options.limits.max_nesting_depth as u64,
            });
        }

        let mut probe = backing.reader()?;
        let format = detect(&mut probe, name, &options.types)?
            .ok_or_else(|| VfsError::unsupported(format!("no archive format for {name}")))?;
        drop(probe);

        if !format.is_supported() {
            return Err(VfsError::unsupported(format!(
                "{} archives are not readable in this build",
                format.label()
            )));
        }

        // Enumerating the container is one operation and spends an allowance
        // of its own; what a later read of an entry costs is charged to that
        // read, not to whatever scanning already used.
        let scan = Budget::new(options.limits.max_archive_bytes);
        let cancel = cancel.clone();
        let tree = match format {
            ArchiveFormat::Zip => {
                let entries = zip_format::list(&backing, options.password.as_deref(), &scan)?;
                let tree = Tree::build(entries);
                return Ok(Self::assemble(
                    backing,
                    format,
                    name,
                    Backend::Zip,
                    tree,
                    options,
                ));
            }
            ArchiveFormat::Tar
            | ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz => {
                let stream = tar_format::stream_format(format);
                let entries = tar_format::list(&backing, stream, &options.limits, &scan, &cancel)?;
                let tree = Tree::build(entries);
                return Ok(Self::assemble(
                    backing,
                    format,
                    name,
                    Backend::Tar(stream),
                    tree,
                    options,
                ));
            }
            ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz => {
                let stream = single::SingleStream::new(format, name);
                let entries = stream.list(&backing)?;
                let tree = Tree::build(entries);
                return Ok(Self::assemble(
                    backing,
                    format,
                    name,
                    Backend::Single(stream),
                    tree,
                    options,
                ));
            }
            ArchiveFormat::SevenZip => {
                let entries = sevenz_format::list(&backing, options.password.as_deref(), &cancel)?;
                Tree::build(entries)
            }
            ArchiveFormat::DiskImage | ArchiveFormat::Deb | ArchiveFormat::Rpm => {
                let (index, entries) = match format {
                    ArchiveFormat::DiskImage => iso_format::list(&backing, &cancel)?,
                    ArchiveFormat::Deb => {
                        deb_format::list(&backing, &options.limits, &scan, &cancel)?
                    }
                    _ => rpm_format::list(&backing, &options.limits, &scan, &cancel)?,
                };
                let tree = Tree::build(entries);
                return Ok(Self::assemble(
                    backing,
                    format,
                    name,
                    Backend::Spans(index),
                    tree,
                    options,
                ));
            }
            ArchiveFormat::Cab => {
                let (index, entries) = cab_format::list(&backing, &cancel)?;
                let tree = Tree::build(entries);
                return Ok(Self::assemble(
                    backing,
                    format,
                    name,
                    Backend::Cab(index),
                    tree,
                    options,
                ));
            }
            ArchiveFormat::Rar => {
                let converted = rar_format::convert(&backing, &options.limits, &cancel)?;
                let listing = Budget::new(options.limits.max_archive_bytes);
                let entries =
                    tar_format::list(&converted, None, &options.limits, &listing, &cancel)?;
                let tree = Tree::build(entries);
                return Ok(Self::assemble(
                    backing,
                    format,
                    name,
                    Backend::Rar(converted),
                    tree,
                    options,
                ));
            }
            other => {
                return Err(VfsError::unsupported(format!(
                    "{} archives are not readable in this build",
                    other.label()
                )))
            }
        };

        Ok(Self::assemble(
            backing,
            format,
            name,
            Backend::SevenZip,
            tree,
            options,
        ))
    }

    fn assemble(
        backing: ArchiveBacking,
        format: ArchiveFormat,
        name: &str,
        backend: Backend,
        mut tree: Tree,
        options: ArchiveOptions,
    ) -> Self {
        tree.place_local_stamps(options.zone_offset_seconds);
        let cache = sevenz_format::BlockCache::new(options.limits.memory_spill_bytes);
        let stamp = if format == ArchiveFormat::Zip {
            zip_format::Stamp::of(&backing).ok().flatten()
        } else {
            None
        };
        Self {
            backing,
            format,
            label: name.to_owned(),
            backend: RwLock::new(backend),
            tree: RwLock::new(tree),
            options,
            tar_source: Mutex::new(None),
            sevenz_cache: Mutex::new(cache),
            stamp: Mutex::new(stamp),
            write_lock: Mutex::new(()),
            rewrites: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The recorded state of the container file, for a write to check against.
    fn current_stamp(&self) -> VfsResult<Option<zip_format::Stamp>> {
        match self.stamp.lock() {
            Ok(stamp) => Ok(stamp.clone()),
            Err(_) => Err(VfsError::corrupt("archive stamp lock poisoned")),
        }
    }

    /// A fresh allowance for one caller-facing operation.
    fn operation_budget(&self) -> Budget {
        Budget::new(self.options.limits.max_archive_bytes)
    }

    /// Number of entries in the cached listing, including implied directories.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.with_tree(crate::tree::Tree::len).unwrap_or(0)
    }

    /// Bytes the cached listing holds.
    ///
    /// A folder comparison keeps a listing of each side open for as long as
    /// the comparison is, so what one entry costs is worth being able to
    /// measure. The figure counts the records, their strings and the vectors
    /// linking them, and excludes allocator overhead.
    #[must_use]
    pub fn listing_bytes(&self) -> usize {
        self.with_tree(crate::tree::Tree::heap_bytes).unwrap_or(0)
    }

    /// The format the container was opened as.
    #[must_use]
    pub fn format(&self) -> ArchiveFormat {
        self.format
    }

    /// Open a container stored inside this one.
    ///
    /// The inner container is expanded first, into memory while it is below
    /// [`Limits::memory_spill_bytes`] and into a temporary file past it, since
    /// every backing format needs to seek over the whole container.
    ///
    /// # Errors
    /// Returns [`VfsError::LimitExceeded`] when the nesting depth or an
    /// expansion ceiling is passed, and whatever [`ArchiveFs::open`] reports
    /// for the inner bytes.
    pub fn open_nested(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<Self> {
        let depth = self.options.depth + 1;
        if depth > self.options.limits.max_nesting_depth {
            return Err(VfsError::LimitExceeded {
                kind: LimitKind::NestingDepth,
                limit: self.options.limits.max_nesting_depth as u64,
            });
        }
        let mut open = self.open(path, cancel)?;
        let backing = backing_from_reader(&mut open, &self.options.limits)?;
        let name = path.name().unwrap_or_default().to_owned();
        let mut options = self.options.clone();
        options.depth = depth;
        Self::open_cancellable(backing, &name, options, cancel)
    }

    /// Replace the cached listing after a write.
    fn reload(&self) -> VfsResult<()> {
        let entries = zip_format::list(
            &self.backing,
            self.options.password.as_deref(),
            &self.operation_budget(),
        )?;
        let mut tree = Tree::build(entries);
        tree.place_local_stamps(self.options.zone_offset_seconds);
        match self.backend.write() {
            Ok(mut backend) => *backend = Backend::Zip,
            Err(_) => return Err(VfsError::corrupt("archive index lock poisoned")),
        }
        match self.tree.write() {
            Ok(mut current) => *current = tree,
            Err(_) => return Err(VfsError::corrupt("archive listing lock poisoned")),
        }
        match self.stamp.lock() {
            Ok(mut stamp) => *stamp = zip_format::Stamp::of(&self.backing).ok().flatten(),
            Err(_) => return Err(VfsError::corrupt("archive stamp lock poisoned")),
        }
        Ok(())
    }

    fn with_tree<T>(&self, f: impl FnOnce(&Tree) -> T) -> VfsResult<T> {
        match self.tree.read() {
            Ok(tree) => Ok(f(&tree)),
            Err(_) => Err(VfsError::corrupt("archive listing lock poisoned")),
        }
    }

    /// Whether this container accepts writes: a zip that lives in a real file.
    fn is_writable(&self) -> bool {
        self.format == ArchiveFormat::Zip && self.backing.path().is_some()
    }

    /// A seekable source holding the plain tar, decompressing it once.
    ///
    /// A compressed tar has no index, so serving a later entry by re-running
    /// the decoder from the container's first byte costs the whole stream per
    /// open. The decoded stream is kept instead, in a temporary file that goes
    /// away with this file system.
    fn tar_source(
        &self,
        stream: Option<ArchiveFormat>,
        cancel: &Cancel,
    ) -> VfsResult<ArchiveBacking> {
        if stream.is_none() {
            return Ok(self.backing.clone());
        }
        let mut cached = self
            .tar_source
            .lock()
            .map_err(|_| VfsError::corrupt("archive stream lock poisoned"))?;
        if let Some(existing) = cached.as_ref() {
            return Ok(existing.clone());
        }
        let decoded = tar_format::decode_to_temp(
            &self.backing,
            stream,
            &self.options.limits,
            &self.operation_budget(),
            cancel,
        )?;
        *cached = Some(decoded.clone());
        Ok(decoded)
    }

    /// Apply one batch of changes with a single rewrite of the container.
    ///
    /// An empty batch rewrites nothing. The whole batch lands or none of it
    /// does, because the rewrite is written beside the container and renamed
    /// over it.
    ///
    /// Each edit names a row of the listing. A write to a file row replaces
    /// the content of that row's record and keeps the record's stored name.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] for a container this build does not
    /// write, [`VfsError::IsADirectory`] for a write to a directory row,
    /// [`VfsError::AlreadyExists`] for a directory added over a file row,
    /// [`VfsError::ContainerChanged`] when the file on disk no longer
    /// matches what was opened, and [`VfsError::Io`] when the rewrite fails.
    pub fn apply_edits(&self, edits: &[ArchiveEdit], cancel: &Cancel) -> VfsResult<()> {
        self.require_writable()?;
        if edits.is_empty() {
            return Ok(());
        }
        let mutations = self.with_tree(|tree| {
            edits
                .iter()
                .map(|edit| edit.to_mutation(tree))
                .collect::<VfsResult<Vec<zip_format::Mutation>>>()
        })??;
        self.apply(&mutations, cancel)
    }

    /// How many times this container has been rewritten.
    ///
    /// A batch that rewrites once reports one, whatever the number of changes
    /// in it.
    #[must_use]
    pub fn rewrite_count(&self) -> u64 {
        self.rewrites.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn apply(&self, mutations: &[zip_format::Mutation], cancel: &Cancel) -> VfsResult<()> {
        let target = self.require_writable()?.to_path_buf();
        let guard = self
            .write_lock
            .lock()
            .map_err(|_| VfsError::corrupt("archive write lock poisoned"))?;
        let result = zip_format::rewrite(
            &self.backing,
            &target,
            mutations,
            self.options.password.as_deref(),
            self.current_stamp()?.as_ref(),
            self.options.zone_offset_seconds,
            cancel,
        );
        drop(guard);
        result?;
        self.rewrites
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.reload()
    }

    fn require_writable(&self) -> VfsResult<&Path> {
        if self.format != ArchiveFormat::Zip {
            return Err(VfsError::unsupported(format!(
                "{} archives are read-only",
                self.format.label()
            )));
        }
        self.backing.path().ok_or(VfsError::ReadOnly)
    }
}

impl FileSystem for ArchiveFs {
    /// Every format but a single compressed stream lists a modification time.
    /// A zip entry without an NTFS or an extended timestamp field, and every
    /// cabinet entry, lists a DOS stamp: two seconds, in the zone the
    /// container is opened with. Every other time is an instant at a whole
    /// second. A zip record that an edit adds carries the time the edit
    /// names, and a rewrite keeps the time fields of every record it keeps.
    ///
    /// The timestamp flag states what the listing holds and no comparison
    /// reads it: the file of a single compressed stream lists no time, and
    /// that absent time keeps the quick tests from settling the pair.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: self.is_writable(),
            supports_timestamps: !matches!(
                self.format,
                ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz
            ),
            supports_attributes: matches!(
                self.format,
                ArchiveFormat::Zip
                    | ArchiveFormat::Tar
                    | ArchiveFormat::TarGz
                    | ArchiveFormat::TarBz2
                    | ArchiveFormat::TarXz
                    | ArchiveFormat::SevenZip
                    | ArchiveFormat::Cab
                    | ArchiveFormat::DiskImage
                    | ArchiveFormat::Deb
                    | ArchiveFormat::Rpm
                    | ArchiveFormat::Rar
            ),
            stored_crc: matches!(self.format, ArchiveFormat::Zip | ArchiveFormat::SevenZip),
            random_access: true,
            content_available: true,
        }
    }

    fn root_label(&self) -> String {
        self.label.clone()
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        cancel.check()?;
        if let Some(entries) = self.with_tree(|tree| tree.list(dir))? {
            return Ok(entries);
        }
        let exists = self.with_tree(|tree| tree.get(dir).is_some())?;
        if exists {
            Err(VfsError::NotADirectory { path: dir.clone() })
        } else {
            Err(VfsError::NotFound { path: dir.clone() })
        }
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        if path.is_root() {
            return Ok(VfsEntry::directory(VfsPath::root()));
        }
        self.with_tree(|tree| tree.get(path).cloned())?
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })
    }

    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile> {
        cancel.check()?;
        let entry = self.metadata(path)?;
        if entry.kind == EntryKind::Directory {
            return Err(VfsError::IsADirectory { path: path.clone() });
        }
        if entry.refused {
            return Err(VfsError::Refused {
                path: path.clone(),
                reason: entry.error.unwrap_or_default(),
            });
        }

        let budget = self.operation_budget();
        let backend = self
            .backend
            .read()
            .map_err(|_| VfsError::corrupt("archive index lock poisoned"))?;
        match &*backend {
            Backend::Zip => {
                let locator = self.with_tree(|tree| tree.locator(path))?;
                zip_format::open_entry(
                    &self.backing,
                    locator,
                    path,
                    &entry,
                    self.options.password.as_deref(),
                    &self.options.limits,
                    &budget,
                    cancel,
                )
            }
            Backend::Tar(stream) => {
                let stream = *stream;
                let offset = self.with_tree(|tree| tree.locator(path))?;
                drop(backend);
                let source = self.tar_source(stream, cancel)?;
                tar_format::open_entry(
                    &source,
                    offset,
                    path,
                    entry.size,
                    &self.options.limits,
                    &budget,
                    cancel,
                )
            }
            Backend::Single(single) => {
                single.open_entry(&self.backing, path, &self.options.limits, &budget, cancel)
            }
            Backend::Spans(index) => {
                let locator = self.with_tree(|tree| tree.locator(path))?;
                index.open_entry(locator, path, &self.options.limits, &budget, cancel)
            }
            Backend::Cab(index) => {
                let locator = self.with_tree(|tree| tree.locator(path))?;
                index.open_entry(
                    &self.backing,
                    locator,
                    path,
                    &self.options.limits,
                    &budget,
                    cancel,
                )
            }
            Backend::Rar(converted) => {
                let offset = self.with_tree(|tree| tree.locator(path))?;
                tar_format::open_entry(
                    converted,
                    offset,
                    path,
                    entry.size,
                    &self.options.limits,
                    &budget,
                    cancel,
                )
            }
            Backend::SevenZip => {
                let locator = self.with_tree(|tree| tree.locator(path))?;
                sevenz_format::open_entry(
                    &self.backing,
                    locator,
                    path,
                    self.options.password.as_deref(),
                    &self.options.limits,
                    &budget,
                    cancel,
                    &self.sevenz_cache,
                )
            }
        }
    }

    fn create_dir(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        self.require_writable()?;
        let mutation = self.with_tree(|tree| added_directory(tree, path))??;
        self.apply(&[mutation], cancel)
    }

    fn write_file(&self, path: &VfsPath, content: &mut dyn Read, cancel: &Cancel) -> VfsResult<()> {
        if path.is_root() {
            return Err(VfsError::IsADirectory { path: path.clone() });
        }
        self.require_writable()?;
        let replaces = self.with_tree(|tree| written(tree, path))??;
        let mut bytes = Vec::new();
        content.read_to_end(&mut bytes)?;
        self.apply(
            &[zip_format::Mutation::WriteFile {
                path: path.clone(),
                replaces,
                content: bytes,
                modified: None,
            }],
            cancel,
        )
    }

    fn delete(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        if path.is_root() {
            return Err(VfsError::unsupported("deleting the root of an archive"));
        }
        self.require_writable()?;
        self.metadata(path)?;
        let entry = self.with_tree(|tree| selected(tree, path))?;
        self.apply(&[zip_format::Mutation::Delete { entry }], cancel)
    }

    fn rename(&self, from: &VfsPath, to: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        if from.is_root() || to.is_root() {
            return Err(VfsError::unsupported("renaming the root of an archive"));
        }
        self.require_writable()?;
        self.metadata(from)?;
        if from == to {
            return Ok(());
        }
        if self.with_tree(|tree| tree.get(to).is_some())? {
            return Err(VfsError::AlreadyExists { path: to.clone() });
        }
        let from = self.with_tree(|tree| selected(tree, from))?;
        self.apply(
            &[zip_format::Mutation::Rename {
                from,
                to: to.clone(),
                replaces: None,
            }],
            cancel,
        )
    }

    /// A container names each entry once, so a path is its own identity and a
    /// walk of one can never revisit a branch.
    fn link_identity(&self, path: &VfsPath) -> Option<String> {
        Some(path.as_str().to_owned())
    }
}

/// Drain an open entry into a backing a nested container can seek over.
fn backing_from_reader(open: &mut OpenFile, limits: &Limits) -> VfsResult<ArchiveBacking> {
    use std::io::Write;

    let mut memory = Vec::new();
    let mut temp: Option<tempfile::NamedTempFile> = None;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = open.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let slice = chunk.get(..read).unwrap_or_default();
        if let Some(file) = temp.as_mut() {
            file.write_all(slice)?;
            continue;
        }
        memory.extend_from_slice(slice);
        if memory.len() as u64 > limits.memory_spill_bytes {
            let mut file = tempfile::NamedTempFile::new()?;
            file.write_all(&memory)?;
            memory = Vec::new();
            temp = Some(file);
        }
    }
    match temp {
        Some(mut file) => {
            file.flush()?;
            Ok(ArchiveBacking::Temp(Arc::new(file)))
        }
        None => Ok(ArchiveBacking::from_bytes(memory)),
    }
}

/// Seconds since the Unix epoch for a civil date and time read as UTC.
///
/// A DOS wall clock read this way is moved into the zone the container is
/// opened with once the listing is built.
pub(crate) fn civil_to_unix(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
}

/// Convert seconds since the Unix epoch into a system time.
pub(crate) fn unix_to_system_time(seconds: i64) -> Option<std::time::SystemTime> {
    let epoch = std::time::SystemTime::UNIX_EPOCH;
    if seconds >= 0 {
        epoch.checked_add(std::time::Duration::from_secs(seconds.unsigned_abs()))
    } else {
        epoch.checked_sub(std::time::Duration::from_secs(seconds.unsigned_abs()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn epoch_conversion_matches_known_instants() {
        assert_eq!(civil_to_unix(1970, 1, 1, 0, 0, 0), Some(0));
        assert_eq!(civil_to_unix(2000, 3, 1, 0, 0, 0), Some(951_868_800));
        assert_eq!(civil_to_unix(2024, 2, 29, 12, 0, 0), Some(1_709_208_000));
        assert_eq!(civil_to_unix(2024, 13, 1, 0, 0, 0), None);
    }
}
