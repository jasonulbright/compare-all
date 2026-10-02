//! One side of a folder comparison, whatever it is stored in.
//!
//! A side is a file system and a root inside it. A local folder, a container
//! read as folders, a recorded listing and a remote location all reach the
//! engine the same way, so scanning, the quick tests and the content tests are
//! written once.
//!
//! The local folder keeps its own scanner. [`Source::is_local_folder`] names
//! that case, and [`crate::scan::scan_source`] takes the path scanner for it
//! rather than walking the trait.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ca_vfs::{
    ArchiveFs, ArchiveOptions, Cancel as VfsCancel, Capabilities, FileSystem, LocalFs, SnapshotFs,
    VfsPath,
};

pub use ca_vfs::{ArchiveFormat, ArchiveHandling, ArchiveTypes, Limits, TimeFidelity};

/// What one side of a comparison is stored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// A directory on a local or mounted volume.
    LocalFolder,
    /// A container read as folders.
    Archive,
    /// A listing captured earlier. It holds no content.
    Snapshot,
    /// A location reached over a network protocol.
    Remote,
}

impl SourceKind {
    /// A word for the kind, used in messages and in journal records.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::LocalFolder => "folder",
            Self::Archive => "archive",
            Self::Snapshot => "snapshot",
            Self::Remote => "remote",
        }
    }
}

/// Facts about one entry that decide what a listing-only test may conclude.
///
/// A scan of a local folder produces the default for every entry, so the tests
/// behave exactly as they did before sources existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryFacts {
    /// False when the size is a hint the source could not prove. An inexact
    /// size proves neither equality nor difference.
    pub size_is_exact: bool,
    /// How much the modification time can be trusted.
    pub time_fidelity: TimeFidelity,
    /// Checksum the source stored with the entry, never one this crate took.
    pub crc32: Option<u32>,
}

impl Default for EntryFacts {
    fn default() -> Self {
        Self {
            size_is_exact: true,
            time_fidelity: TimeFidelity::Utc,
            crc32: None,
        }
    }
}

impl EntryFacts {
    /// The facts a listing record carries.
    #[must_use]
    pub fn of(entry: &ca_vfs::VfsEntry) -> Self {
        Self {
            size_is_exact: entry.size_is_exact,
            time_fidelity: entry.time_fidelity,
            crc32: entry.crc32,
        }
    }
}

/// Errors raised while opening a source.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The underlying file system refused to open.
    #[error("cannot open {label}: {detail}")]
    Open {
        /// What was being opened.
        label: String,
        /// Text of the underlying failure.
        detail: String,
    },
}

/// One side of a comparison: a file system and a root inside it.
#[derive(Clone)]
pub struct Source {
    kind: SourceKind,
    fs: Arc<dyn FileSystem>,
    /// The same file system again where it is a container, so a nested
    /// container and a batched rewrite reach it without a downcast.
    archive: Option<Arc<ArchiveFs>>,
    root: VfsPath,
    /// The path a local folder occupies, or the container file of an archive.
    /// A source with no path on this machine leaves it empty.
    origin: PathBuf,
    label: String,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("root", &self.root.as_str())
            .finish_non_exhaustive()
    }
}

impl Source {
    /// A local folder.
    #[must_use]
    pub fn local(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Self {
            kind: SourceKind::LocalFolder,
            fs: Arc::new(LocalFs::new(path)),
            archive: None,
            root: VfsPath::root(),
            origin: path.to_path_buf(),
            label: path.display().to_string(),
        }
    }

    /// A container read as folders.
    ///
    /// # Errors
    /// Returns [`SourceError::Open`] when the bytes name no readable format or
    /// the container does not parse.
    pub fn archive(path: impl AsRef<Path>, options: ArchiveOptions) -> Result<Self, SourceError> {
        let path = path.as_ref();
        let fs = ArchiveFs::open_path(path, options).map_err(|error| SourceError::Open {
            label: path.display().to_string(),
            detail: error.to_string(),
        })?;
        let fs = Arc::new(fs);
        Ok(Self {
            kind: SourceKind::Archive,
            archive: Some(Arc::clone(&fs)),
            fs,
            root: VfsPath::root(),
            origin: path.to_path_buf(),
            label: path.display().to_string(),
        })
    }

    /// A listing captured earlier.
    ///
    /// # Errors
    /// Returns [`SourceError::Open`] when the file is not a readable capture.
    pub fn snapshot(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        let path = path.as_ref();
        let fs = SnapshotFs::open(path).map_err(|error| SourceError::Open {
            label: path.display().to_string(),
            detail: error.to_string(),
        })?;
        Ok(Self {
            kind: SourceKind::Snapshot,
            archive: None,
            fs: Arc::new(fs),
            root: VfsPath::root(),
            origin: path.to_path_buf(),
            label: path.display().to_string(),
        })
    }

    /// A source over a file system the caller already built, such as a remote
    /// profile location.
    #[must_use]
    pub fn over(kind: SourceKind, fs: Arc<dyn FileSystem>) -> Self {
        let label = fs.root_label();
        Self {
            kind,
            archive: None,
            fs,
            root: VfsPath::root(),
            origin: PathBuf::new(),
            label,
        }
    }

    /// Open whatever sits at `path`, expanding a container as the handling
    /// option allows.
    ///
    /// A directory is always a folder. A file is a container when the handling
    /// option asks for it and the name or the bytes name a format this build
    /// reads. `AsFiles` leaves a container file unopened and reports the error
    /// the caller shows. A snapshot opens under every handling: it is a
    /// recorded listing with no file content, so it has no form as a file to
    /// compare. A container reads a DOS stamp in the current zone of the
    /// machine, [`crate::zone::local_offset_seconds`].
    ///
    /// # Errors
    /// Returns [`SourceError::Open`] when the path is neither a directory nor
    /// a source this build can expand.
    pub fn open(
        path: impl AsRef<Path>,
        types: &ArchiveTypes,
        limits: &Limits,
    ) -> Result<Self, SourceError> {
        let path = path.as_ref();
        if path.is_dir() {
            return Ok(Self::local(path));
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_snapshot_name(&name) {
            return Self::snapshot(path);
        }
        match types.handling() {
            ArchiveHandling::AsFiles => Err(SourceError::Open {
                label: path.display().to_string(),
                detail: "containers are not expanded as folders".to_owned(),
            }),
            ArchiveHandling::AsFoldersOnceOpened | ArchiveHandling::AsFoldersAlways => {
                Self::archive(
                    path,
                    ArchiveOptions {
                        limits: *limits,
                        types: types.clone(),
                        password: None,
                        depth: 0,
                        zone_offset_seconds: crate::zone::local_offset_seconds(),
                    },
                )
            }
        }
    }

    /// A source over a container nested inside this one.
    ///
    /// The nesting ceiling of the outer container's limits still holds, so a
    /// chain that reaches it reports a typed limit failure rather than opening.
    ///
    /// # Errors
    /// Returns [`SourceError::Open`] when the entry is not a readable
    /// container, or when opening it would pass a ceiling.
    pub fn nested_archive(&self, path: &VfsPath, cancel: &VfsCancel) -> Result<Self, SourceError> {
        let outer = self.archive.as_ref().ok_or_else(|| SourceError::Open {
            label: path.as_str().to_owned(),
            detail: "only a container holds a nested container".to_owned(),
        })?;
        let nested = outer
            .open_nested(path, cancel)
            .map_err(|error| SourceError::Open {
                label: path.as_str().to_owned(),
                detail: error.to_string(),
            })?;
        let label = format!("{}/{}", self.label, path.as_str());
        let nested = Arc::new(nested);
        Ok(Self {
            kind: SourceKind::Archive,
            archive: Some(Arc::clone(&nested)),
            fs: nested,
            root: VfsPath::root(),
            origin: PathBuf::new(),
            label,
        })
    }

    /// What the source is stored in.
    #[must_use]
    pub const fn kind(&self) -> SourceKind {
        self.kind
    }

    /// The file system behind the source.
    #[must_use]
    pub fn file_system(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// The root inside the file system.
    #[must_use]
    pub const fn root(&self) -> &VfsPath {
        &self.root
    }

    /// The path inside this source of the entry a scan keyed by `rel`.
    ///
    /// # Errors
    /// Returns the rule the joined path breaks.
    pub fn entry_path(&self, rel: &Path) -> Result<VfsPath, ca_vfs::PathError> {
        let mut path = self.root.clone();
        for part in rel.components() {
            path = path.join(&part.as_os_str().to_string_lossy())?;
        }
        Ok(path)
    }

    /// The path this source occupies on this machine: the folder itself, or
    /// the container file. Empty for a source with no local path.
    #[must_use]
    pub fn origin(&self) -> &Path {
        &self.origin
    }

    /// A label for the root.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// What the file system can do.
    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        self.fs.capabilities()
    }

    /// True when the side is a plain local directory, which has its own
    /// scanner.
    #[must_use]
    pub fn is_local_folder(&self) -> bool {
        self.kind == SourceKind::LocalFolder
    }

    /// True when the source accepts writes.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.capabilities().writable
    }

    /// True when the source stores the bytes of its files.
    #[must_use]
    pub fn has_content(&self) -> bool {
        self.capabilities().content_available
    }

    /// Why a destructive step on this source is refused, or `None` when it can
    /// run.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        if self.is_writable() {
            return None;
        }
        Some(match self.kind {
            SourceKind::Snapshot => "a recorded listing holds no files to change".to_owned(),
            SourceKind::Archive => format!("{} is a read-only container", self.label),
            SourceKind::LocalFolder | SourceKind::Remote => {
                format!("{} refuses writes", self.label)
            }
        })
    }
}

/// True when a name is a recorded listing rather than a container.
fn is_snapshot_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cass"))
}

impl Source {
    /// The container behind this source, where it is one.
    ///
    /// A caller uses it to apply a batch of changes with a single rewrite.
    #[must_use]
    pub fn as_archive(&self) -> Option<&Arc<ArchiveFs>> {
        self.archive.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{EntryFacts, Source, SourceKind};
    use ca_vfs::TimeFidelity;

    #[test]
    fn a_local_folder_keeps_the_path_scanner() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::local(dir.path());
        assert!(source.is_local_folder());
        assert_eq!(source.kind(), SourceKind::LocalFolder);
        assert!(source.is_writable());
        assert!(source.refusal().is_none());
        assert_eq!(source.origin(), dir.path());
    }

    #[test]
    fn default_facts_prove_as_much_as_a_local_listing_does() {
        let facts = EntryFacts::default();
        assert!(facts.size_is_exact);
        assert_eq!(facts.time_fidelity, TimeFidelity::Utc);
        assert!(facts.crc32.is_none());
    }

    #[test]
    fn every_kind_has_a_word() {
        for kind in [
            SourceKind::LocalFolder,
            SourceKind::Archive,
            SourceKind::Snapshot,
            SourceKind::Remote,
        ] {
            assert!(!kind.word().is_empty());
        }
    }
}
