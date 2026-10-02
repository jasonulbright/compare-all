//! Recorded folder listings.
//!
//! A snapshot stores what a folder tree looked like: names, sizes, times,
//! attributes and, when asked for, a CRC32 of each file. It stores no content,
//! so it is small enough to keep for a whole volume and compare against later.
//! Because content is absent, a snapshot opened as a file system lists like
//! any other source but fails every open with [`VfsError::ContentNotStored`].

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{LimitKind, VfsError, VfsResult};
use crate::fs::{walk, Capabilities, FileSystem, OpenFile};
use crate::path::VfsPath;
use crate::stored::{refuse, stored_path};

/// Magic bytes at the start of every snapshot file.
pub const MAGIC: &[u8; 8] = b"CA-VFSSN";

/// Version this build writes.
pub const FORMAT_VERSION: u16 = 1;

/// Oldest reader that can still make sense of what this build writes.
pub const MIN_READER_VERSION: u16 = 1;

/// Fixed part of the header, in bytes.
const HEADER_LEN: usize = 24;

/// Payload is stored as a deflate stream.
const COMPRESSION_DEFLATE: u8 = 1;
/// Payload is stored as it is.
const COMPRESSION_NONE: u8 = 0;

/// What to record while capturing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Compute a CRC32 of every file. Slower to capture, but it lets a later
    /// comparison detect a content change the size and time do not show.
    pub include_crc: bool,
    /// Record folders that hold nothing.
    pub include_empty_folders: bool,
    /// Record the entries links point at rather than the links.
    pub follow_links: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            include_crc: false,
            include_empty_folders: true,
            follow_links: false,
        }
    }
}

/// One recorded entry.
///
/// Fields written by a later build are kept in `unknown` and written back out
/// unchanged, so a round trip through this build does not lose them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    /// Path relative to the captured root.
    pub path: String,
    /// True for directories.
    #[serde(default)]
    pub dir: bool,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
    /// Modification time in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<i64>,
    /// How far `modified` can be trusted, where the captured source stated
    /// less than an instant. Absent means an instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_fidelity: Option<RecordedFidelity>,
    /// Creation time in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    /// Attributes where the source had them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<VfsAttributes>,
    /// CRC32 of the content, when the capture asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc32: Option<u32>,
    /// Link kind when the entry is a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<VfsLinkKind>,
    /// Version string for executables, reserved for a later build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_info: Option<String>,
    /// Why the record is incomplete, when something stopped the capture short
    /// of recording everything it was asked for. A record whose CRC could not
    /// be read carries the reason here and no CRC, so a later comparison can
    /// tell "no checksum was asked for" from "the checksum could not be taken".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True when the source listed the entry but refused to open it, so a
    /// later comparison cannot rule on it either. `error` says why.
    #[serde(default, skip_serializing_if = "is_false")]
    pub refused: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde requires a predicate taking a reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// How far a recorded modification time can be trusted, as the payload
/// spells it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordedFidelity {
    /// The time names an instant.
    Utc,
    /// The time is a DOS stamp with a two-second tick, which the captured
    /// container placed in the zone it was opened with.
    LocalTwoSecond,
    /// The time names a minute.
    MinutePrecision,
    /// The time names a day.
    DayPrecision,
    /// A precision this build does not know, carried through unchanged. It
    /// reads as an instant, as it reads in a build that does not know the
    /// field.
    #[serde(untagged)]
    Unknown(Value),
}

impl RecordedFidelity {
    /// The record of `fidelity`, or `None` for an instant, which a record
    /// states by leaving the field out.
    #[must_use]
    pub fn of(fidelity: TimeFidelity) -> Option<Self> {
        match fidelity {
            TimeFidelity::Utc => None,
            TimeFidelity::LocalTwoSecond => Some(Self::LocalTwoSecond),
            TimeFidelity::MinutePrecision => Some(Self::MinutePrecision),
            TimeFidelity::DayPrecision => Some(Self::DayPrecision),
        }
    }

    /// The precision this record states.
    #[must_use]
    pub fn fidelity(&self) -> TimeFidelity {
        match self {
            Self::Utc | Self::Unknown(_) => TimeFidelity::Utc,
            Self::LocalTwoSecond => TimeFidelity::LocalTwoSecond,
            Self::MinutePrecision => TimeFidelity::MinutePrecision,
            Self::DayPrecision => TimeFidelity::DayPrecision,
        }
    }
}

/// A captured listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The folder the capture came from, so a later session can compare
    /// against it without being told where it was.
    pub origin: String,
    /// When the capture ran, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured: Option<i64>,
    /// Whether every file carries a CRC32.
    #[serde(default)]
    pub has_crc: bool,
    /// The recorded entries.
    pub entries: Vec<SnapshotRecord>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

impl Snapshot {
    /// Record everything under `root` of `source`.
    ///
    /// # Errors
    /// Propagates a failure listing the root, and [`VfsError::Cancelled`] when
    /// the flag is raised. A file whose CRC cannot be read is recorded without
    /// one, with the failure on the record's entry error instead of ending the
    /// capture.
    pub fn capture(
        source: &dyn FileSystem,
        root: &VfsPath,
        options: SnapshotOptions,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        let entries = walk(source, root, cancel)?;
        let mut records = Vec::with_capacity(entries.len());
        let mut non_empty = std::collections::BTreeSet::new();
        for entry in &entries {
            if let Some(parent) = entry.path.parent() {
                non_empty.insert(parent);
            }
        }

        for entry in entries {
            cancel.check()?;
            if entry.is_dir() && !options.include_empty_folders && !non_empty.contains(&entry.path)
            {
                continue;
            }
            if entry.is_link() && !options.follow_links && entry.is_dir() {
                // A directory link is recorded as a leaf: its target is
                // reachable by its own path, and following it can loop.
            }
            let mut error = entry.error.clone();
            let crc32 = if options.include_crc
                && !entry.is_dir()
                && !entry.refused
                && entry.crc32.is_none()
            {
                match crc_of(source, &entry.path, cancel) {
                    Ok(value) => Some(value),
                    Err(VfsError::Cancelled) => return Err(VfsError::Cancelled),
                    Err(failure) => {
                        let text = format!("checksum not taken: {failure}");
                        error = Some(match error {
                            Some(existing) => format!("{existing}; {text}"),
                            None => text,
                        });
                        None
                    }
                }
            } else {
                entry.crc32
            };
            records.push(SnapshotRecord {
                path: entry.path.as_str().to_owned(),
                dir: entry.is_dir(),
                size: entry.size,
                modified: entry.modified.and_then(to_unix),
                time_fidelity: RecordedFidelity::of(entry.time_fidelity),
                created: entry.created.and_then(to_unix),
                attributes: entry.attributes,
                crc32,
                link: entry.link,
                version_info: entry.version_info,
                error,
                refused: entry.refused,
                unknown: BTreeMap::new(),
            });
        }

        Ok(Self {
            origin: source.root_label(),
            captured: to_unix(SystemTime::now()),
            has_crc: options.include_crc,
            entries: records,
            unknown: BTreeMap::new(),
        })
    }

    /// Serialize into the container format.
    ///
    /// # Errors
    /// Returns [`VfsError::Io`] when the payload cannot be encoded.
    pub fn write_to(&self, writer: &mut dyn Write) -> VfsResult<()> {
        let payload = serde_json::to_vec(self)
            .map_err(|error| VfsError::corrupt(format!("snapshot encode: {error}")))?;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&payload)?;
        let compressed = encoder.finish()?;

        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        header.extend_from_slice(&MIN_READER_VERSION.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        header.push(COMPRESSION_DEFLATE);
        header.push(0);
        header.extend_from_slice(&(payload.len() as u64).to_le_bytes());

        writer.write_all(&header)?;
        writer.write_all(&compressed)?;
        writer.flush()?;
        Ok(())
    }

    /// Write the container to a file.
    ///
    /// # Errors
    /// Returns [`VfsError::Io`] when the file cannot be written.
    pub fn save(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        ca_io::replace(path.as_ref(), |writer| self.write_to(writer))
    }

    /// Parse the container format.
    ///
    /// A header written by a later build is accepted as long as that build
    /// says this reader's version is enough; unknown header bytes are skipped
    /// and unknown payload fields are preserved.
    ///
    /// # Errors
    /// Returns [`VfsError::Corrupt`] when the magic, the version or the
    /// payload does not check out.
    pub fn read_from(reader: &mut dyn Read) -> VfsResult<Self> {
        Self::read_from_with(reader, &crate::limits::Limits::default())
    }

    /// Parse the container format under explicit ceilings.
    ///
    /// Nothing in the header is evidence. The declared payload length and the
    /// compressed body are both bounded before anything is allocated, so a
    /// small file cannot make this build reserve a large buffer or inflate an
    /// unbounded stream.
    ///
    /// # Errors
    /// Returns [`VfsError::Corrupt`] when the magic, the version or the
    /// payload does not check out, and [`VfsError::LimitExceeded`] when the
    /// payload is larger than `limits` allows.
    pub fn read_from_with(
        reader: &mut dyn Read,
        limits: &crate::limits::Limits,
    ) -> VfsResult<Self> {
        let mut header = [0u8; HEADER_LEN];
        reader
            .read_exact(&mut header)
            .map_err(|_| VfsError::corrupt("snapshot header is truncated"))?;
        if header.get(..8) != Some(MAGIC.as_slice()) {
            return Err(VfsError::corrupt("not a snapshot"));
        }
        let format_version = read_u16(&header, 8)?;
        let min_reader = read_u16(&header, 10)?;
        if min_reader > FORMAT_VERSION {
            return Err(VfsError::corrupt(format!(
                "snapshot needs a reader for version {min_reader}, this build reads {FORMAT_VERSION}"
            )));
        }
        let extra = usize::from(read_u16(&header, 12)?);
        let compression = *header.get(14).unwrap_or(&COMPRESSION_NONE);
        let declared = read_u64(&header, 16)?;

        if extra > 0 {
            let mut skip = vec![0u8; extra];
            reader
                .read_exact(&mut skip)
                .map_err(|_| VfsError::corrupt("snapshot header extension is truncated"))?;
        }

        // The ceiling is whichever is smaller: what the writer said the
        // payload comes to, and what this build will hold for one payload at
        // all. A declared length is refused before it is believed, so a file
        // claiming an enormous payload costs nothing until the bytes arrive.
        let ceiling = if declared == 0 {
            limits.max_entry_bytes
        } else {
            declared.min(limits.max_entry_bytes)
        };
        if declared > limits.max_entry_bytes {
            return Err(VfsError::LimitExceeded {
                kind: LimitKind::EntrySize,
                limit: limits.max_entry_bytes,
            });
        }

        let payload = match compression {
            COMPRESSION_NONE => read_capped(reader, ceiling)?,
            COMPRESSION_DEFLATE => {
                let body = read_capped(reader, limits.max_entry_bytes)?;
                let mut decoder = flate2::read::DeflateDecoder::new(body.as_slice());
                read_capped(&mut decoder, ceiling)?
            }
            other => {
                return Err(VfsError::corrupt(format!(
                    "snapshot compression {other} is not known to this build"
                )))
            }
        };
        if declared != 0 && declared != payload.len() as u64 {
            return Err(VfsError::corrupt("snapshot payload length disagrees"));
        }
        let _ = format_version;

        serde_json::from_slice(&payload)
            .map_err(|error| VfsError::corrupt(format!("snapshot payload: {error}")))
    }

    /// Read the container from a file.
    ///
    /// # Errors
    /// Returns [`VfsError::Io`] or [`VfsError::Corrupt`].
    pub fn load(path: impl AsRef<Path>) -> VfsResult<Self> {
        let mut file = std::fs::File::open(path)?;
        Self::read_from(&mut file)
    }
}

/// A saved listing presented as a read-only file system whose files hold no
/// content.
#[derive(Debug)]
pub struct SnapshotFs {
    snapshot: Snapshot,
    tree: crate::tree::Tree,
}

impl SnapshotFs {
    /// Present `snapshot` as a file system.
    #[must_use]
    pub fn new(snapshot: Snapshot) -> Self {
        let mut entries = Vec::with_capacity(snapshot.entries.len());

        for record in &snapshot.entries {
            let Some((path, refusal)) = stored_path(&record.path, record.dir).into_row() else {
                continue;
            };
            let name = path.name().unwrap_or_default().to_owned();
            let mut entry = VfsEntry {
                path: path.clone(),
                name,
                kind: if record.dir {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                size: record.size,
                size_is_exact: true,
                modified: record.modified.and_then(from_unix),
                time_fidelity: record
                    .time_fidelity
                    .as_ref()
                    .map_or(TimeFidelity::Utc, RecordedFidelity::fidelity),
                created: record.created.and_then(from_unix),
                attributes: record.attributes,
                crc32: record.crc32,
                link: record.link,
                version_info: record.version_info.clone(),
                error: record.error.clone(),
                refused: record.refused,
            };
            if let Some(reason) = refusal {
                refuse(&mut entry, &reason);
            }
            entries.push(entry);
        }

        Self {
            snapshot,
            tree: crate::tree::Tree::build(entries),
        }
    }

    /// Read a snapshot file and present it as a file system.
    ///
    /// # Errors
    /// Returns whatever [`Snapshot::load`] reports.
    pub fn open(path: impl AsRef<Path>) -> VfsResult<Self> {
        Ok(Self::new(Snapshot::load(path)?))
    }

    /// The recorded listing.
    #[must_use]
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
}

impl FileSystem for SnapshotFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: false,
            supports_timestamps: true,
            supports_attributes: true,
            stored_crc: self.snapshot.has_crc,
            random_access: false,
            content_available: false,
        }
    }

    fn root_label(&self) -> String {
        self.snapshot.origin.clone()
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        cancel.check()?;
        if let Some(entries) = self.tree.list(dir) {
            return Ok(entries);
        }
        if self.tree.get(dir).is_some() {
            Err(VfsError::NotADirectory { path: dir.clone() })
        } else {
            Err(VfsError::NotFound { path: dir.clone() })
        }
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        if path.is_root() {
            return Ok(VfsEntry::directory(VfsPath::root()));
        }
        self.tree
            .get(path)
            .cloned()
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })
    }

    fn open(&self, path: &VfsPath, _cancel: &Cancel) -> VfsResult<OpenFile> {
        match self.tree.get(path) {
            Some(entry) if entry.is_dir() => Err(VfsError::IsADirectory { path: path.clone() }),
            Some(entry) if entry.refused => Err(VfsError::Refused {
                path: path.clone(),
                reason: entry.error.clone().unwrap_or_default(),
            }),
            Some(_) => Err(VfsError::ContentNotStored { path: path.clone() }),
            None => Err(VfsError::NotFound { path: path.clone() }),
        }
    }

    /// A recorded listing holds each path once, so a path is its own identity
    /// and a walk of one can never revisit a branch.
    fn link_identity(&self, path: &VfsPath) -> Option<String> {
        Some(path.as_str().to_owned())
    }
}

/// Read at most `cap` bytes, failing rather than growing past it.
///
/// The buffer grows as bytes arrive instead of being reserved from a declared
/// length, so a claim costs nothing until it is paid for.
fn read_capped(reader: &mut dyn Read, cap: u64) -> VfsResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| VfsError::corrupt(format!("snapshot payload: {error}")))?;
        if read == 0 {
            return Ok(out);
        }
        if out.len() as u64 + read as u64 > cap {
            return Err(VfsError::LimitExceeded {
                kind: LimitKind::EntrySize,
                limit: cap,
            });
        }
        out.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
}

/// CRC32 of one file's content.
fn crc_of(source: &dyn FileSystem, path: &VfsPath, cancel: &Cancel) -> VfsResult<u32> {
    let mut open = source.open(path, cancel)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let read = open.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hasher.update(chunk.get(..read).unwrap_or_default());
    }
    Ok(hasher.finalize())
}

fn to_unix(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => i64::try_from(delta.as_secs()).ok(),
        Err(error) => i64::try_from(error.duration().as_secs()).ok().map(|s| -s),
    }
}

fn from_unix(seconds: i64) -> Option<SystemTime> {
    if seconds >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    }
}

fn read_u16(header: &[u8], offset: usize) -> VfsResult<u16> {
    let slice = header
        .get(offset..offset + 2)
        .ok_or_else(|| VfsError::corrupt("snapshot header is truncated"))?;
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(slice);
    Ok(u16::from_le_bytes(bytes))
}

fn read_u64(header: &[u8], offset: usize) -> VfsResult<u64> {
    let slice = header
        .get(offset..offset + 8)
        .ok_or_else(|| VfsError::corrupt("snapshot header is truncated"))?;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(slice);
    Ok(u64::from_le_bytes(bytes))
}
