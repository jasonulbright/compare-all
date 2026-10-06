//! The primitive file system calls every destructive step is built from.
//!
//! Every call the executor makes goes through [`FileOps`], so a batch can be
//! driven against an implementation that fails a chosen call and the recovery
//! path can be exercised without arranging a real disk fault.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// A destination file that can be forced to stable storage before it is
/// renamed over its target.
pub trait SyncWrite: Write + Send {
    /// Flush the file's contents and metadata to stable storage.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn sync_data(&mut self) -> io::Result<()>;
}

impl SyncWrite for File {
    fn sync_data(&mut self) -> io::Result<()> {
        File::sync_all(self)
    }
}

/// Platform error codes that mean the volume holding the item has no recycle
/// bin: a share, a removable volume that is not ready, or a file system the
/// shell refuses to trash on.
const NO_RECYCLE_BIN_CODES: &[i32] = &[
    1,  // the volume does not support the operation
    21, // the device is not ready
    50, // the request is not supported
    53, // the network path was not found
    67, // the network name is not available
];

/// A Windows shell failure arrives as an `HRESULT` wrapping a system code.
fn system_code(code: i32) -> i32 {
    #[allow(clippy::cast_possible_wrap)]
    let facility_win32 = 0x8007_0000_u32 as i32;
    if code < 0 && (code & !0xffff) == facility_win32 {
        code & 0xffff
    } else {
        code
    }
}

/// The error kind a failed trash call reports.
///
/// [`io::ErrorKind::Unsupported`] means the volume has no recycle bin, which is
/// the one failure an outright removal can stand in for. Every other failure
/// keeps a kind that stops the step.
fn trash_error_kind(error: &trash::Error) -> io::ErrorKind {
    match error {
        trash::Error::Os { code, .. } if NO_RECYCLE_BIN_CODES.contains(&system_code(*code)) => {
            io::ErrorKind::Unsupported
        }
        #[cfg(all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        ))]
        // No trash directory exists on the volume holding the item.
        trash::Error::FileSystem { source, .. } if source.kind() == io::ErrorKind::NotFound => {
            io::ErrorKind::Unsupported
        }
        trash::Error::CouldNotAccess { .. } => io::ErrorKind::NotFound,
        trash::Error::TargetedRoot => io::ErrorKind::PermissionDenied,
        _ => io::ErrorKind::Other,
    }
}

/// True when `path` lies on a volume the shell does not recycle on: a share,
/// a mapped network drive, removable media or an optical disc.
///
/// The shell deletes an item on such a volume outright and still reports
/// success, so the trash call never reaches the codes above.
#[cfg(windows)]
fn lacks_recycle_bin(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    let on_share = |path: &Path| {
        matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
        )
    };
    if on_share(path) {
        return true;
    }
    let Ok(absolute) = std::path::absolute(path) else {
        return false;
    };
    // The item may be a link to another volume; its own entry lives in the
    // volume of its parent folder.
    let resolved = absolute
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .unwrap_or(absolute);
    if on_share(&resolved) {
        return true;
    }
    let Some(text) = resolved.to_str() else {
        return false;
    };
    let Ok(root) = winsafe::GetVolumePathName(text) else {
        return false;
    };
    let root = match root.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        Some(rest) if rest.starts_with(r"UNC\") => return true,
        None if root.starts_with(r"\\") => return true,
        _ => root,
    };
    matches!(
        winsafe::GetDriveType(Some(&root)),
        winsafe::co::DRIVE::REMOTE | winsafe::co::DRIVE::REMOVABLE | winsafe::co::DRIVE::CDROM
    )
}

#[cfg(not(windows))]
fn lacks_recycle_bin(_path: &Path) -> bool {
    false
}

/// Attribute edits to apply to one item.
///
/// `None` leaves the attribute as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeChange {
    /// Target state of the read-only flag, or the owner write bit off Windows.
    pub read_only: Option<bool>,
    /// Target state of the hidden flag.
    pub hidden: Option<bool>,
    /// Target state of the archive flag.
    pub archive: Option<bool>,
    /// Target state of the system flag. The folder view's dialog does not
    /// offer it; a script's `attrib` command sets it.
    #[serde(default)]
    pub system: Option<bool>,
    /// Target permission bits on platforms with a Unix mode.
    pub unix_mode: Option<u32>,
}

impl AttributeChange {
    /// True when nothing would change.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.read_only.is_none()
            && self.hidden.is_none()
            && self.archive.is_none()
            && self.system.is_none()
            && self.unix_mode.is_none()
    }
}

/// What the file system reports about one path, read without following links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetState {
    /// True when the path is a directory, or a link whose target is one.
    pub is_dir: bool,
    /// True when the path itself is a symbolic link, junction or other
    /// reparse point.
    pub is_link: bool,
    /// True when writes are refused by the read-only flag or the owner write
    /// bit.
    pub read_only: bool,
    /// True when the platform marks the item hidden.
    pub hidden: bool,
    /// Size in bytes; zero for directories.
    pub size: u64,
    /// Last modification time when the platform reports one.
    pub modified: Option<SystemTime>,
    /// Creation time when the platform reports one.
    pub created: Option<SystemTime>,
}

/// The item a path reaches through its links, named by the volume that holds
/// it and by its number on that volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemIdentity {
    /// The serial number of the volume on Windows, the device number
    /// elsewhere.
    pub volume: u64,
    /// The file index on Windows, the inode number elsewhere.
    pub number: u64,
}

/// The file system calls a plan step is built from.
pub trait FileOps: Send + Sync {
    /// Read a path's state without following a link.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::NotFound`] when nothing is there.
    fn probe(&self, path: &Path) -> io::Result<TargetState>;

    /// Create one directory; the parent must already exist.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn create_dir(&self, path: &Path) -> io::Result<()>;

    /// Open a file for reading.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>>;

    /// Create a file that must not already exist.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::AlreadyExists`] when the path is taken.
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>>;

    /// Rename a path, replacing an existing file at the destination.
    ///
    /// # Errors
    /// Propagates the underlying I/O error. A cross-volume rename fails here
    /// and the caller falls back to copying.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Rename a path onto a name that no item holds.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::AlreadyExists`] when an item holds `to`, and
    /// propagates every other error.
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        check_then_rename(self, from, to)
    }

    /// Write the bytes of `reader` as a new file at `target`, a name that no
    /// item may hold.
    ///
    /// The bytes go to an owned temporary beside `target`, which takes the
    /// name only when no item holds it. A failure after the temporary was
    /// created removes the temporary.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::AlreadyExists`] when an item holds `target`,
    /// and propagates every other error.
    fn write_new(
        &self,
        target: &Path,
        reader: &mut dyn Read,
        buffer_size: usize,
    ) -> io::Result<()> {
        write_new_through(self, target, reader, buffer_size)
    }

    /// Remove one file, or the link itself when the path is a link.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Remove one empty directory, or the link itself when the path is a
    /// directory link.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn remove_dir(&self, path: &Path) -> io::Result<()>;

    /// Hand a path to the platform's trash facility.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::Unsupported`] where no trash facility is
    /// reachable, which the caller treats as a reason to fall back or to stop.
    fn move_to_trash(&self, path: &Path) -> io::Result<()>;

    /// Set a path's last modification time.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()>;

    /// Set a path's creation time where the platform stores one.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::Unsupported`] where creation time cannot be
    /// written.
    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()>;

    /// Apply an attribute change.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()>;

    /// True when a rename between the two paths can stay a rename.
    ///
    /// A false answer only costs a copy, so an implementation that cannot tell
    /// answers false.
    fn same_volume(&self, from: &Path, to: &Path) -> bool;

    /// List a directory's immediate children.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;

    /// Resolve a path to the real location it names, following every link in
    /// it.
    ///
    /// Containment cannot be judged on path text, because any component may be
    /// a link planted after the plan was built.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    /// The identity of the item `path` reaches through its links, or `None`
    /// where the file system states none.
    ///
    /// A share of a local folder, a mapped drive and a link reach an item
    /// under text of their own, which [`FileOps::canonicalize`] does not
    /// fold into one location. The identity of the item is the same through
    /// each of them.
    ///
    /// # Errors
    /// Propagates the error of an open that fails.
    fn identity(&self, path: &Path) -> io::Result<Option<ItemIdentity>> {
        local_identity(path)
    }

    /// An opaque identity for the volume itself, distinct from a file system's
    /// serial number which a cloned volume may share. Equal identities name
    /// one volume; different identities prove two volumes. No answer preserves
    /// the conservative file-identity checks.
    ///
    /// # Errors
    /// Propagates an error reading the volume identity.
    fn volume_identity(&self, _path: &Path) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// True when the file system that holds `path` keeps the case of every
    /// name, so two names that differ by case only name two items.
    ///
    /// A container keeps case. A local volume answers false, because whether
    /// it folds case is a property of each volume.
    fn keeps_case(&self, _path: &Path) -> bool {
        false
    }

    /// True when `path` names an entry of a source that names each entry by
    /// one path, so the location [`FileOps::canonicalize`] gives is the
    /// identity of the entry and no second route reaches it.
    ///
    /// Such an entry is never an item of the local disk. A local path
    /// answers false.
    fn identity_is_path(&self, _path: &Path) -> bool {
        false
    }
}

/// What a file system shows about whether two paths reach one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// The two reach two items.
    TwoItems,
    /// The two reach one item.
    OneItem,
    /// The file system states no identity for the two, their exact locations
    /// differ, and nothing read proves two items. This includes paths that differ
    /// only by case on a local volume whose case behavior is unknown.
    Unproven,
}

/// What `ops` shows about whether `first` and `second` reach one item.
///
/// An entry of a source that names each entry by one path is one item with
/// another such entry when their locations are one, and never one item with
/// a local path. Two identities that differ are two items. An identity is
/// the volume serial number and the file index, and two volumes can carry
/// one serial number, so two equal identities prove one item only while the
/// states do not prove two items and the paths resolve together or share a
/// volume. Where the file system states no identity, exact paths that resolve
/// to one location are one item.
/// Paths that match only after folding case are unproven unless their states
/// prove two items.
pub(crate) fn same_item<F: FileOps + ?Sized>(ops: &F, first: &Path, second: &Path) -> Reach {
    let folds = !(ops.keeps_case(first) && ops.keeps_case(second));
    match (ops.identity_is_path(first), ops.identity_is_path(second)) {
        (true, true) if one_location(ops, first, second, folds) => return Reach::OneItem,
        (true, _) | (_, true) => return Reach::TwoItems,
        (false, false) => {}
    }
    if let (Ok(Some(one)), Ok(Some(other))) = (ops.identity(first), ops.identity(second)) {
        if one != other || states_differ(ops, first, second) {
            return Reach::TwoItems;
        }
        if one_location(ops, first, second, false) {
            return Reach::OneItem;
        }
        if let (Ok(Some(one_volume)), Ok(Some(other_volume))) =
            (ops.volume_identity(first), ops.volume_identity(second))
        {
            return if one_volume == other_volume {
                Reach::OneItem
            } else {
                Reach::TwoItems
            };
        }
        return if ops.same_volume(first, second) {
            Reach::OneItem
        } else {
            Reach::Unproven
        };
    }
    if one_location(ops, first, second, false) {
        return Reach::OneItem;
    }
    if states_differ(ops, first, second) {
        return Reach::TwoItems;
    }
    if folds && one_location(ops, first, second, true) {
        return Reach::Unproven;
    }
    let names_differ = matches!(
        (first.file_name(), second.file_name()),
        (Some(one), Some(other)) if !same_spelling(Path::new(one), Path::new(other), folds)
    );
    if names_differ {
        return Reach::TwoItems;
    }
    Reach::Unproven
}

/// True when the two paths resolve to one location through their links,
/// without case when `folds`. A path that does not resolve is compared as
/// written.
fn one_location<F: FileOps + ?Sized>(ops: &F, first: &Path, second: &Path, folds: bool) -> bool {
    match (ops.canonicalize(first), ops.canonicalize(second)) {
        (Ok(first), Ok(second)) => same_spelling(&first, &second, folds),
        _ => same_spelling(first, second, folds),
    }
}

/// True when the states `ops` reads for the two paths prove two items: a
/// different kind, size or modification time.
///
/// The first path is read before and after the second. An item that changes
/// between the two reads proves nothing, because one item written to while
/// it is read gives two states.
fn states_differ<F: FileOps + ?Sized>(ops: &F, first: &Path, second: &Path) -> bool {
    let (Ok(before), Ok(other), Ok(after)) =
        (ops.probe(first), ops.probe(second), ops.probe(first))
    else {
        return false;
    };
    before == after
        && (before.is_dir != other.is_dir
            || before.size != other.size
            || before.modified != other.modified)
}

/// The identity of the local item `path` reaches through its links.
///
/// A file index of zero or of all ones states no identity: a file system
/// with no file numbers reports zero, and a volume with 128-bit file numbers
/// reports all ones for every file whose number does not fit in 64 bits.
#[cfg(windows)]
pub(crate) fn local_identity(path: &Path) -> io::Result<Option<ItemIdentity>> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    // No access right is asked for, so a file that another handle holds with
    // no sharing still opens; the backup flag is what opens a folder.
    let file = File::options()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let information = winapi_util::file::information(&file)?;
    let number = information.file_index();
    if number == 0 || number == u64::MAX {
        return Ok(None);
    }
    Ok(Some(ItemIdentity {
        volume: information.volume_serial_number(),
        number,
    }))
}

/// The identity of the local item `path` reaches through its links.
#[cfg(unix)]
pub(crate) fn local_identity(path: &Path) -> io::Result<Option<ItemIdentity>> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)?;
    Ok(Some(ItemIdentity {
        volume: metadata.dev(),
        number: metadata.ino(),
    }))
}

/// The identity of the local item `path` reaches through its links.
#[cfg(not(any(windows, unix)))]
pub(crate) fn local_identity(_path: &Path) -> io::Result<Option<ItemIdentity>> {
    Ok(None)
}

fn same_spelling(first: &Path, second: &Path, folds_case: bool) -> bool {
    if folds_case {
        first.to_string_lossy().to_lowercase() == second.to_string_lossy().to_lowercase()
    } else {
        first == second
    }
}

/// Rename `from` onto `to` after a check that no item holds `to`.
///
/// The check and rename are separate operations: an item that another process
/// creates at `to` between the two is replaced. The standard library has no
/// rename that refuses an existing target.
pub(crate) fn check_then_rename<F: FileOps + ?Sized>(
    ops: &F,
    from: &Path,
    to: &Path,
) -> io::Result<()> {
    match ops.probe(to) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => ops.rename(from, to),
        Err(error) => Err(error),
    }
}

/// [`FileOps::write_new`] built from the other calls of `ops`: a check that no
/// item holds `target`, a temporary written through `ops`, and
/// [`FileOps::rename_no_replace`].
///
/// A taken name is refused before anything is written, so no write of a
/// container batch is queued over an item.
pub(crate) fn write_new_through<F: FileOps + ?Sized>(
    ops: &F,
    target: &Path,
    reader: &mut dyn Read,
    buffer_size: usize,
) -> io::Result<()> {
    match ops.probe(target) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", target.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let temporary = crate::ops::exec::temporary_path(target);
    let mut writer = ops.create_new(&temporary)?;
    let written = copy_all(reader, &mut *writer, buffer_size)
        .and_then(|()| writer.flush())
        .and_then(|()| writer.sync_data());
    drop(writer);
    let committed = written.and_then(|()| ops.rename_no_replace(&temporary, target));
    if committed.is_err() {
        let _ = ops.remove_file(&temporary);
    }
    committed
}

/// [`FileOps::write_new`] on the local disk, through
/// [`ca_io::replace_new_checked`]. `check` runs immediately before each attempt
/// to rename the temporary.
pub(crate) fn write_new_local(
    target: &Path,
    reader: &mut dyn Read,
    buffer_size: usize,
    check: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    ca_io::replace_new_checked(
        target,
        |written: &mut dyn Write| copy_all(reader, written, buffer_size),
        check,
    )
}

fn copy_all<W: Write + ?Sized>(
    reader: &mut dyn Read,
    writer: &mut W,
    buffer_size: usize,
) -> io::Result<()> {
    let mut buffer = vec![0u8; buffer_size.max(4096)];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..read])?;
    }
}

/// [`FileOps`] backed by the real file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFs;

impl FileOps for RealFs {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        let meta = std::fs::symlink_metadata(path)?;
        let is_link = meta.file_type().is_symlink() || windows_reparse_point(&meta);
        let is_dir = if is_link {
            std::fs::metadata(path).map_or(meta.is_dir(), |target| target.is_dir())
        } else {
            meta.is_dir()
        };
        Ok(TargetState {
            is_dir,
            is_link,
            read_only: meta.permissions().readonly(),
            hidden: windows_hidden(&meta),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            created: meta.created().ok(),
        })
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        match std::fs::create_dir(path) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
            other => other,
        }
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(File::open(path)?))
    }

    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        Ok(Box::new(
            File::options().write(true).create_new(true).open(path)?,
        ))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    #[cfg(windows)]
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let from = win32_path(from)?;
        let to = win32_path(to)?;
        // Without REPLACE_EXISTING in the flags, the call itself fails with
        // ERROR_ALREADY_EXISTS when an item holds the new name, so no other
        // process can take the name between a check and the rename.
        winsafe::MoveFileEx(&from, Some(&to), winsafe::co::MOVEFILE::default()).map_err(win_error)
    }

    fn write_new(
        &self,
        target: &Path,
        reader: &mut dyn Read,
        buffer_size: usize,
    ) -> io::Result<()> {
        write_new_local(target, reader, buffer_size, || Ok(()))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }

    fn move_to_trash(&self, path: &Path) -> io::Result<()> {
        if lacks_recycle_bin(path) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{}: this location has no recycle bin", path.display()),
            ));
        }
        trash::delete(path).map_err(|error| {
            io::Error::new(
                trash_error_kind(&error),
                format!("{}: {error}", path.display()),
            )
        })
    }

    #[cfg(windows)]
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        open_for_times(path)?.set_times(std::fs::FileTimes::new().set_modified(time))
    }

    #[cfg(not(windows))]
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(time))
    }

    #[cfg(windows)]
    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        use std::os::windows::fs::FileTimesExt;
        open_for_times(path)?.set_times(std::fs::FileTimes::new().set_created(time))
    }

    #[cfg(not(windows))]
    fn set_created(&self, _path: &Path, _time: SystemTime) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()> {
        set_attributes_impl(path, change)
    }

    fn same_volume(&self, from: &Path, to: &Path) -> bool {
        volume_key(from).is_some_and(|left| volume_key(to).is_some_and(|right| left == right))
    }

    fn volume_identity(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        local_volume_identity(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            out.push(entry?.path());
        }
        Ok(out)
    }
}

/// Opens a file or a directory for a timestamp write only.
///
/// `FILE_WRITE_ATTRIBUTES` is the least authority that carries a timestamp
/// write, and it is granted on a read-only file, which a write handle is not.
/// `FILE_FLAG_BACKUP_SEMANTICS` is what makes the same call work on a
/// directory.
#[cfg(windows)]
fn open_for_times(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    File::options()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(windows)]
fn windows_bits(meta: &std::fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
fn windows_bits(_meta: &std::fs::Metadata) -> u32 {
    0
}

fn windows_reparse_point(meta: &std::fs::Metadata) -> bool {
    const REPARSE_POINT: u32 = 0x0000_0400;
    windows_bits(meta) & REPARSE_POINT != 0
}

fn windows_hidden(meta: &std::fs::Metadata) -> bool {
    const HIDDEN: u32 = 0x0000_0002;
    windows_bits(meta) & HIDDEN != 0
}

/// Read the mount manager's identity rather than the file system serial. Mount
/// points and drive letters for one volume hold the same opaque binary value.
/// Resolving the path first follows links into their actual mounted volume.
#[cfg(windows)]
fn local_volume_identity(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use winsafe::prelude::advapi_Hkey;
    let canonical = std::fs::canonicalize(path)?;
    let Ok(root) = winsafe::GetVolumePathName(&canonical.to_string_lossy()) else {
        return Ok(None);
    };
    let root = root
        .strip_prefix(r"\\?\")
        .unwrap_or(&root)
        .trim_end_matches('\\');
    let name = if root.starts_with("Volume{") {
        format!(r"\??\{root}")
    } else {
        format!(r"\DosDevices\{root}")
    };
    let value = winsafe::HKEY::LOCAL_MACHINE.RegGetValue(
        Some(r"SYSTEM\MountedDevices"),
        Some(&name),
        winsafe::co::RRF::RT_REG_BINARY,
    );
    Ok(match value {
        Ok(winsafe::RegistryValue::Binary(bytes)) if !bytes.is_empty() => Some(bytes),
        _ => None,
    })
}

#[cfg(unix)]
fn local_volume_identity(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(std::fs::metadata(path)?.dev().to_le_bytes().to_vec()))
}

#[cfg(not(any(windows, unix)))]
fn local_volume_identity(_path: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

/// A volume is identified by the path prefix on Windows and by the device id
/// on Unix; an answer that cannot be formed means the caller copies instead of
/// renaming, which is always correct.
#[cfg(windows)]
fn volume_key(path: &Path) -> Option<String> {
    use std::path::Component;
    let absolute = absolute_path(path)?;
    absolute.components().next().and_then(|first| match first {
        Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().to_lowercase()),
        _ => None,
    })
}

#[cfg(not(windows))]
fn volume_key(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mut probe = path;
    loop {
        if let Ok(meta) = std::fs::metadata(probe) {
            return Some(meta.dev().to_string());
        }
        probe = probe.parent()?;
    }
}

#[cfg(windows)]
fn absolute_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    std::env::current_dir().ok().map(|cwd| cwd.join(path))
}

#[cfg(windows)]
fn set_attributes_impl(path: &Path, change: &AttributeChange) -> io::Result<()> {
    use winsafe::co::FILE_ATTRIBUTE;

    if change.is_empty() {
        return Ok(());
    }
    // The read-only flag is the only attribute the standard library exposes.
    // A change that touches nothing else therefore needs no platform call.
    if change.hidden.is_none() && change.archive.is_none() && change.system.is_none() {
        let Some(read_only) = change.read_only else {
            return Ok(());
        };
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(read_only);
        return std::fs::set_permissions(path, permissions);
    }

    // Attribute calls take a fully qualified name; the canonical form is also
    // the form that survives the 260 character limit.
    let canonical = std::fs::canonicalize(path)?;
    let text = canonical.to_string_lossy().to_string();
    let current = winsafe::GetFileAttributes(&text).map_err(win_error)?;

    let mut bits = current;
    let mut apply = |flag: FILE_ATTRIBUTE, wanted: Option<bool>| match wanted {
        Some(true) => bits |= flag,
        Some(false) => bits &= !flag,
        None => {}
    };
    apply(FILE_ATTRIBUTE::READONLY, change.read_only);
    apply(FILE_ATTRIBUTE::HIDDEN, change.hidden);
    apply(FILE_ATTRIBUTE::ARCHIVE, change.archive);
    apply(FILE_ATTRIBUTE::SYSTEM, change.system);

    if bits == current {
        return Ok(());
    }
    // A zero attribute word is rejected; NORMAL is the documented stand-in for
    // "no flags set".
    let bits = if bits.raw() == 0 {
        FILE_ATTRIBUTE::NORMAL
    } else {
        bits
    };
    winsafe::SetFileAttributes(&text, bits).map_err(win_error)
}

#[cfg(windows)]
fn win_error(code: winsafe::co::ERROR) -> io::Error {
    io::Error::from_raw_os_error(i32::try_from(code.raw()).unwrap_or(-1))
}

/// `path` as the text a wide-character Windows call takes.
///
/// Such a call does not lift the legacy path limit as the standard library
/// does, so a path at that limit or past it is made absolute and given the
/// verbatim prefix. A path that is not valid Unicode is refused rather than
/// converted with loss.
#[cfg(windows)]
fn win32_path(path: &Path) -> io::Result<String> {
    const LEGACY_MAX_PATH: usize = 248;
    let text = |path: &Path| {
        path.to_str().map(str::to_owned).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not valid Unicode", path.display()),
            )
        })
    };
    let verbatim = |text: &str| text.starts_with(r"\\?\") || text.starts_with(r"\\.\");
    let plain = text(path)?;
    if plain.encode_utf16().count() < LEGACY_MAX_PATH || verbatim(&plain) {
        return Ok(plain);
    }
    let absolute = text(&std::path::absolute(path)?)?;
    if verbatim(&absolute) {
        return Ok(absolute);
    }
    Ok(match absolute.strip_prefix(r"\\") {
        Some(share) => format!(r"\\?\UNC\{share}"),
        None => format!(r"\\?\{absolute}"),
    })
}

#[cfg(not(windows))]
fn set_attributes_impl(path: &Path, change: &AttributeChange) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if change.is_empty() {
        return Ok(());
    }
    let mut permissions = std::fs::metadata(path)?.permissions();
    if let Some(mode) = change.unix_mode {
        permissions.set_mode(mode);
    } else if let Some(read_only) = change.read_only {
        permissions.set_readonly(read_only);
    }
    std::fs::set_permissions(path, permissions)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{same_item, trash_error_kind, AttributeChange, FileOps, Reach, RealFs};

    #[cfg(windows)]
    #[test]
    fn a_share_has_no_recycle_bin_and_a_fixed_drive_has_one() {
        use std::path::Path;
        for share in [
            r"\\server.invalid\share\a.txt",
            r"\\?\UNC\server.invalid\share\a.txt",
        ] {
            assert!(super::lacks_recycle_bin(Path::new(share)), "{share}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(!super::lacks_recycle_bin(&dir.path().join("a.txt")));
    }

    #[test]
    fn a_volume_without_a_recycle_bin_reports_an_unsupported_trash_call() {
        for code in [1, 21, 50, 53, 67] {
            let error = trash::Error::Os {
                code,
                description: "no recycle bin".to_owned(),
            };
            assert_eq!(
                trash_error_kind(&error),
                std::io::ErrorKind::Unsupported,
                "system code {code}"
            );
            let wrapped = trash::Error::Os {
                #[allow(clippy::cast_possible_wrap)]
                code: (0x8007_0000_u32 as i32) | code,
                description: "no recycle bin".to_owned(),
            };
            assert_eq!(
                trash_error_kind(&wrapped),
                std::io::ErrorKind::Unsupported,
                "wrapped system code {code}"
            );
        }
    }

    #[test]
    fn any_other_trash_failure_keeps_a_kind_that_stops_the_step() {
        let denied = trash::Error::Os {
            code: 5,
            description: "access denied".to_owned(),
        };
        assert_ne!(trash_error_kind(&denied), std::io::ErrorKind::Unsupported);
        assert_ne!(
            trash_error_kind(&trash::Error::TargetedRoot),
            std::io::ErrorKind::Unsupported
        );
        assert_eq!(
            trash_error_kind(&trash::Error::CouldNotAccess {
                target: "x".to_owned()
            }),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn a_rename_that_must_not_replace_leaves_a_taken_name_alone() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.txt");
        let file = dir.path().join("b.txt");
        let folder = dir.path().join("folder");
        std::fs::write(&from, b"a").unwrap();
        std::fs::write(&file, b"b").unwrap();
        std::fs::create_dir(&folder).unwrap();

        for taken in [&file, &folder] {
            let error = RealFs.rename_no_replace(&from, taken).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error}");
        }
        assert_eq!(std::fs::read(&from).unwrap(), b"a");
        assert_eq!(std::fs::read(&file).unwrap(), b"b");
        assert!(folder.is_dir());

        let free = dir.path().join("c.txt");
        RealFs.rename_no_replace(&from, &free).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read(&free).unwrap(), b"a");
    }

    /// A path past the legacy limit reaches the Windows call through the
    /// verbatim prefix.
    #[cfg(windows)]
    #[test]
    fn a_rename_that_must_not_replace_reaches_a_long_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().to_path_buf();
        while deep.as_os_str().len() < 300 {
            deep.push("a-folder-name-that-is-long");
        }
        std::fs::create_dir_all(&deep).unwrap();
        let from = deep.join("a.txt");
        let taken = deep.join("b.txt");
        std::fs::write(&from, b"a").unwrap();
        std::fs::write(&taken, b"b").unwrap();

        let error = RealFs.rename_no_replace(&from, &taken).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error}");
        assert_eq!(std::fs::read(&taken).unwrap(), b"b");

        let free = deep.join("c.txt");
        RealFs.rename_no_replace(&from, &free).unwrap();
        assert_eq!(std::fs::read(&free).unwrap(), b"a");
    }

    /// Two names of one file resolve to two locations, and the file they
    /// name is one item.
    #[test]
    fn two_names_of_one_file_are_one_item() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.txt");
        let second = dir.path().join("b.txt");
        let other = dir.path().join("c.txt");
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(&other, b"x").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        assert_eq!(same_item(&RealFs, &first, &second), Reach::OneItem);
        assert_eq!(same_item(&RealFs, &first, &other), Reach::TwoItems);
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn the_local_volume_identity_is_shared_by_files_on_one_mount() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.txt");
        let second = dir.path().join("b.txt");
        std::fs::write(&first, b"a").unwrap();
        std::fs::write(&second, b"b").unwrap();
        let volume = RealFs.volume_identity(&first).unwrap();
        assert!(volume.is_some());
        assert_eq!(volume, RealFs.volume_identity(&second).unwrap());
    }

    #[test]
    fn probe_reports_a_missing_path_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let error = RealFs.probe(&dir.path().join("absent")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn read_only_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        RealFs
            .set_attributes(
                &file,
                &AttributeChange {
                    read_only: Some(true),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
        assert!(RealFs.probe(&file).unwrap().read_only);
        RealFs
            .set_attributes(
                &file,
                &AttributeChange {
                    read_only: Some(false),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
        assert!(!RealFs.probe(&file).unwrap().read_only);
    }

    #[cfg(unix)]
    #[test]
    fn a_unix_mode_change_reaches_the_permission_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        RealFs
            .set_attributes(
                &file,
                &AttributeChange {
                    unix_mode: Some(0o640),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o640);
    }

    #[test]
    fn same_volume_holds_within_one_temporary_tree() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        assert!(RealFs.same_volume(dir.path(), &nested));
    }

    #[test]
    fn empty_attribute_change_is_a_no_op() {
        assert!(AttributeChange::default().is_empty());
    }

    /// A stamp far enough from now that a coarse file system clock cannot make
    /// the comparison pass by accident.
    fn stamp() -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000)
    }

    #[test]
    fn modified_time_round_trips_on_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        RealFs.set_modified(&file, stamp()).unwrap();
        assert_eq!(RealFs.probe(&file).unwrap().modified, Some(stamp()));
    }

    #[test]
    fn modified_time_round_trips_on_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        RealFs.set_modified(&nested, stamp()).unwrap();
        assert_eq!(RealFs.probe(&nested).unwrap().modified, Some(stamp()));
    }

    #[cfg(windows)]
    #[test]
    fn creation_time_round_trips_on_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        RealFs.set_created(&file, stamp()).unwrap();
        assert_eq!(RealFs.probe(&file).unwrap().created, Some(stamp()));
    }

    #[cfg(windows)]
    #[test]
    fn creation_time_round_trips_on_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        RealFs.set_created(&nested, stamp()).unwrap();
        assert_eq!(RealFs.probe(&nested).unwrap().created, Some(stamp()));
    }

    #[cfg(windows)]
    #[test]
    fn creation_time_is_set_on_a_read_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        RealFs
            .set_attributes(
                &file,
                &AttributeChange {
                    read_only: Some(true),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
        RealFs.set_created(&file, stamp()).unwrap();
        assert_eq!(RealFs.probe(&file).unwrap().created, Some(stamp()));
    }

    #[cfg(windows)]
    #[test]
    fn modified_time_is_set_on_a_read_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        RealFs
            .set_attributes(
                &file,
                &AttributeChange {
                    read_only: Some(true),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
        RealFs.set_modified(&file, stamp()).unwrap();
        assert_eq!(RealFs.probe(&file).unwrap().modified, Some(stamp()));
    }

    #[cfg(windows)]
    #[test]
    fn hidden_and_archive_flags_change_independently() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        RealFs
            .set_attributes(
                &file,
                &AttributeChange {
                    hidden: Some(true),
                    archive: Some(false),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
        let state = RealFs.probe(&file).unwrap();
        assert!(state.hidden);
        assert!(!state.read_only);

        RealFs
            .set_attributes(
                &file,
                &AttributeChange {
                    hidden: Some(false),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
        assert!(!RealFs.probe(&file).unwrap().hidden);
    }

    #[cfg(windows)]
    #[test]
    fn a_change_of_the_system_flag_alone_reaches_the_file() {
        use std::os::windows::fs::MetadataExt;

        const SYSTEM: u32 = 0x0000_0004;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let system = |wanted: bool| AttributeChange {
            system: Some(wanted),
            ..AttributeChange::default()
        };
        assert!(!system(true).is_empty());

        RealFs.set_attributes(&file, &system(true)).unwrap();
        let bits = std::fs::metadata(&file).unwrap().file_attributes();
        assert_ne!(bits & SYSTEM, 0, "attributes {bits:#x}");

        RealFs.set_attributes(&file, &system(false)).unwrap();
        let bits = std::fs::metadata(&file).unwrap().file_attributes();
        assert_eq!(bits & SYSTEM, 0, "attributes {bits:#x}");
    }
}
