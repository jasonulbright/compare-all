//! One trait over every browsable source: the local disk, a container read as
//! folders, and a recorded listing.
//!
//! A folder comparison does not care whether a side is a directory, a zip or a
//! listing captured last month, so each of those implements [`FileSystem`].
//! Paths inside a file system are [`VfsPath`]: relative, forward-slash and
//! validated, so a name taken from a container can never resolve outside the
//! root it is extracted into.
//!
//! Containers are expanded under the ceilings in [`Limits`], which bound what
//! a deliberately over-compressed container can cost, and every long running
//! call takes a [`Cancel`] so a scan started on a worker can be stopped.
//!
//! ```no_run
//! use ca_vfs::{Cancel, FileSystem, LocalFs, VfsPath};
//!
//! let fs = LocalFs::new(".");
//! let entries = fs.list(&VfsPath::root(), &Cancel::new())?;
//! println!("{} entries", entries.len());
//! # Ok::<(), ca_vfs::VfsError>(())
//! ```

pub mod archive;
pub mod cancel;
pub mod detect;
pub mod entry;
pub mod error;
pub mod fs;
pub mod limits;
pub mod local;
pub mod path;
pub mod remote;
pub mod snapshot;
pub(crate) mod stored;
#[cfg(feature = "testing")]
pub mod testing;
pub(crate) mod tree;

pub use archive::{ArchiveBacking, ArchiveEdit, ArchiveFs, ArchiveOptions};
pub use cancel::Cancel;
pub use detect::{detect, split_masks, ArchiveFormat, ArchiveHandling, ArchiveTypes};
pub use entry::{EntryKind, TimeFidelity, VfsAttributes, VfsEntry, VfsLinkKind};
pub use error::{LimitKind, VfsError, VfsResult};
pub use fs::{walk, Capabilities, FileSystem, OpenFile};
pub use limits::Limits;
pub use local::LocalFs;
pub use path::{PathError, VfsPath, MAX_COMPONENTS, MAX_PATH_BYTES};
#[cfg(feature = "os-keychain")]
pub use remote::OsSecretStore;
#[cfg(feature = "tls")]
pub use remote::TlsOptions;
pub use remote::{
    KnownHosts, MemorySecretStore, RemoteContext, RemoteProfile, Secret, SecretRef, SecretStore,
    ServiceProfile,
};
pub use snapshot::{Snapshot, SnapshotFs, SnapshotOptions};
pub use stored::{free_spelling, platform_refusal};
