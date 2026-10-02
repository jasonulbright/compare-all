//! Listing records produced by every file system implementation.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::path::VfsPath;

/// File system attributes captured for one entry, where the source has them.
///
/// The four flags are the ones a folder view renders as letters. A source that
/// stores no attributes leaves them all false and both raw words `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a record of independent file system flags"
)]
pub struct VfsAttributes {
    /// Write access is denied by a file system flag or by the owner write bit.
    pub read_only: bool,
    /// Hidden flag on Windows, leading period elsewhere.
    pub hidden: bool,
    /// System flag; false where the platform has none.
    pub system: bool,
    /// Archive flag; false where the platform has none.
    pub archive: bool,
    /// Raw Windows attribute word when the source supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_bits: Option<u32>,
    /// Raw Unix mode word when the source supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,
    /// Unix owner id when the source supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Unix group id when the source supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
}

impl VfsAttributes {
    /// The permission bits of [`VfsAttributes::unix_mode`], without the file
    /// type bits.
    #[must_use]
    pub fn permission_bits(&self) -> Option<u32> {
        self.unix_mode.map(|mode| mode & 0o7777)
    }
}

/// How an entry is linked into the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VfsLinkKind {
    /// A link whose target is a file, or whose target kind is unknown.
    FileLink,
    /// A link whose target is a directory.
    DirectoryLink,
}

/// How much a recorded timestamp can be trusted.
///
/// Some container formats store a wall clock with no zone and a two-second
/// tick, so a comparison against a live folder has to allow a tolerance rather
/// than treat the value as an instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeFidelity {
    /// The stamp names an instant in UTC.
    #[default]
    Utc,
    /// The stamp is a DOS wall clock, rounded to two seconds, which states no
    /// zone. A container reads it in the zone it is opened with
    /// (`ArchiveOptions::zone_offset_seconds`).
    LocalTwoSecond,
    /// The stamp names a minute. The seconds are not stated, so a comparison
    /// allows a tolerance of one minute.
    ///
    /// A plain file transfer listing states a clock with no seconds. The
    /// server's zone offset, where the profile names one, is already applied.
    MinutePrecision,
    /// The stamp names a day. The time of day is not stated, so a comparison
    /// allows a tolerance of one day.
    ///
    /// A plain file transfer listing drops the clock once a file is more than
    /// six months old.
    DayPrecision,
}

impl TimeFidelity {
    /// How far two stamps of this fidelity may differ and still describe the
    /// same moment, in seconds.
    #[must_use]
    pub const fn tolerance_seconds(self) -> u64 {
        match self {
            Self::Utc => 0,
            Self::LocalTwoSecond => 2,
            Self::MinutePrecision => 60,
            Self::DayPrecision => 86_400,
        }
    }
}

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A leaf holding content.
    File,
    /// A node holding other entries.
    Directory,
}

/// One entry in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsEntry {
    /// Path relative to the root of the file system that produced it.
    pub path: VfsPath,
    /// Final component of [`VfsEntry::path`].
    pub name: String,
    /// Whether the entry is a file or a directory.
    pub kind: EntryKind,
    /// Size in bytes; zero for directories and for sources that store no size.
    pub size: u64,
    /// False when [`VfsEntry::size`] is a hint the container could not prove,
    /// so a comparison that rules a pair different on size alone must read the
    /// content instead.
    #[serde(default = "yes")]
    pub size_is_exact: bool,
    /// Last modification time where the source records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<SystemTime>,
    /// How much [`VfsEntry::modified`] can be trusted.
    #[serde(default, skip_serializing_if = "is_utc")]
    pub time_fidelity: TimeFidelity,
    /// Creation time where the source records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<SystemTime>,
    /// Attributes where the source records them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<VfsAttributes>,
    /// CRC32 of the content where the container stores one, so a comparison
    /// can rule a pair equal without reading either side's bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc32: Option<u32>,
    /// Set when the entry itself is a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<VfsLinkKind>,
    /// Product version string for executables, where the source records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_info: Option<String>,
    /// Text of the error that stopped the entry being read in full. The entry
    /// still carries whatever the listing supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True when the source lists the entry but refuses to open or extract
    /// it, so no comparison can rule on it. [`VfsEntry::error`] says why.
    #[serde(default, skip_serializing_if = "is_false")]
    pub refused: bool,
}

fn yes() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde requires a predicate taking a reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde requires a predicate taking a reference"
)]
fn is_utc(fidelity: &TimeFidelity) -> bool {
    *fidelity == TimeFidelity::Utc
}

impl VfsEntry {
    /// A directory entry with no metadata beyond its path.
    #[must_use]
    pub fn directory(path: VfsPath) -> Self {
        let name = path.name().unwrap_or_default().to_owned();
        Self {
            path,
            name,
            kind: EntryKind::Directory,
            size: 0,
            size_is_exact: true,
            time_fidelity: TimeFidelity::default(),
            modified: None,
            created: None,
            attributes: None,
            crc32: None,
            link: None,
            version_info: None,
            error: None,
            refused: false,
        }
    }

    /// A file entry with a size and no other metadata.
    #[must_use]
    pub fn file(path: VfsPath, size: u64) -> Self {
        let name = path.name().unwrap_or_default().to_owned();
        Self {
            path,
            name,
            kind: EntryKind::File,
            size,
            size_is_exact: true,
            time_fidelity: TimeFidelity::default(),
            modified: None,
            created: None,
            attributes: None,
            crc32: None,
            link: None,
            version_info: None,
            error: None,
            refused: false,
        }
    }

    /// True for directories, whether or not they are links.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    /// True when the entry is a link of either kind.
    #[must_use]
    pub fn is_link(&self) -> bool {
        self.link.is_some()
    }
}
