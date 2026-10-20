//! Getting packs from a release: fetch the list, download and unpack a pack,
//! and remove a pack the pack browser installed.
//!
//! Nothing here opens a network connection itself. A [`PackTransport`] does
//! the transfers, so the program supplies its HTTP transport and the tests
//! supply a local one. Every function blocks and belongs on a worker thread.

use super::host::installed_folders;
use super::index::{
    self, check_entry_name, IndexEntry, PackIndex, MARKER_FILE, MAX_INDEX_BYTES, MAX_ZIP_ENTRIES,
};
use super::load::{self, DirectoryFiles, MAX_FILE_BYTES, MAX_TOTAL_FILE_BYTES};
use super::manifest::{MANIFEST_FILE, MAX_MANIFEST_BYTES};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Prefix of the temporary folders an install works in, inside the packs
/// folder.
pub const TEMPORARY_PREFIX: &str = ".incoming-";

/// Why a transfer failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The server has no such file.
    NotFound,
    /// The transfer failed for the reason given.
    Failed(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("the file does not exist"),
            Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

/// Transfers of release files.
pub trait PackTransport: Send + Sync {
    /// Read a small document into memory, at most `max_bytes` long.
    ///
    /// # Errors
    /// Why the document could not be read.
    fn document(
        &self,
        url: &str,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, FetchError>;

    /// Write a file to `destination`, at most `max_bytes` long. The caller
    /// checks the content: success proves only that bytes arrived.
    ///
    /// # Errors
    /// Why the transfer failed.
    fn file(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), FetchError>;
}

/// Where the files of one release are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackLocations {
    /// The folder of the release files, without a trailing slash.
    pub folder: String,
    /// The page of the release.
    pub page: String,
}

impl PackLocations {
    /// The release whose tag is `v` and `version`.
    #[must_use]
    pub fn for_release(download_base: &str, page_base: &str, version: &str) -> Self {
        Self {
            folder: format!("{}/v{version}", download_base.trim_end_matches('/')),
            page: format!("{}/v{version}", page_base.trim_end_matches('/')),
        }
    }

    /// The address of the pack list.
    #[must_use]
    pub fn index_url(&self) -> String {
        self.file_url(index::INDEX_NAME)
    }

    /// The address of a release file.
    #[must_use]
    pub fn file_url(&self, name: &str) -> String {
        format!("{}/{name}", self.folder)
    }
}

/// Everything the pack browser needs from the program.
#[derive(Clone)]
pub struct PackServices {
    /// The transfers.
    pub transport: Arc<dyn PackTransport>,
    /// The release files.
    pub locations: PackLocations,
    /// The per-user packs folder.
    pub directory: PathBuf,
}

impl std::fmt::Debug for PackServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackServices")
            .field("locations", &self.locations)
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

/// Why the pack list is not available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexFailure {
    /// The release has no pack list.
    NoList,
    /// The list could not be read or was refused.
    Failed(String),
}

/// The identifiers of the packs built into the program.
#[must_use]
pub fn builtin_ids() -> Vec<String> {
    super::host::BUILTIN
        .iter()
        .filter_map(|pack| super::PackManifest::parse(pack.manifest).ok())
        .map(|manifest| manifest.id)
        .collect()
}

/// Fetch and check the pack list of the release.
///
/// # Errors
/// [`IndexFailure::NoList`] when the release has no list, else the reason the
/// list could not be used.
pub fn fetch_index(
    transport: &dyn PackTransport,
    locations: &PackLocations,
    builtin: &[String],
    cancel: &Arc<AtomicBool>,
) -> Result<PackIndex, IndexFailure> {
    let bytes = transport
        .document(&locations.index_url(), MAX_INDEX_BYTES as u64, cancel)
        .map_err(|error| match error {
            FetchError::NotFound => IndexFailure::NoList,
            FetchError::Failed(reason) => IndexFailure::Failed(reason),
        })?;
    let builtin: Vec<&str> = builtin.iter().map(String::as_str).collect();
    index::parse(&bytes, &builtin).map_err(IndexFailure::Failed)
}

/// Fetch the preview image of `entry` and check its size and checksum.
///
/// # Errors
/// Why the image could not be fetched or does not match the list.
pub fn fetch_preview(
    transport: &dyn PackTransport,
    locations: &PackLocations,
    entry: &IndexEntry,
    cancel: &Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    let bytes = transport
        .document(
            &locations.file_url(&entry.preview),
            entry.preview_size,
            cancel,
        )
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != entry.preview_size || hex_digest(&bytes) != entry.preview_sha256 {
        return Err("The preview does not match the pack list.".to_owned());
    }
    Ok(bytes)
}

/// Which step of an install failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStage {
    /// The zip could not be downloaded.
    Download,
    /// The zip arrived and the pack was refused.
    Refused,
}

/// Why an install stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError {
    /// The step that failed.
    pub stage: InstallStage,
    /// What to show.
    pub message: String,
}

impl InstallError {
    fn download(message: String) -> Self {
        Self {
            stage: InstallStage::Download,
            message,
        }
    }

    fn refused(message: impl Into<String>) -> Self {
        Self {
            stage: InstallStage::Refused,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// File inside a work folder that its install holds locked for its whole
/// life. A cleanup in any process deletes a leftover folder only when it can
/// take this lock itself.
const LOCK_FILE: &str = "lock";

/// A temporary folder inside the packs folder. Dropping it deletes it, so a
/// failed install leaves nothing behind.
struct Work {
    path: PathBuf,
    lock: Option<std::fs::File>,
    /// Set when the folder holds the only copy of an earlier pack version.
    keep: std::cell::Cell<bool>,
}

/// Folder inside a work folder that holds the pack being replaced.
const OLD_FOLDER: &str = "old";

impl Work {
    fn create(directory: &Path) -> Result<Self, InstallError> {
        Self::create_named(
            directory,
            std::iter::repeat_with(|| {
                format!(
                    "{TEMPORARY_PREFIX}{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                )
            }),
            &|_| {},
        )
    }

    /// Create a work folder under the first of at most five `names` that is
    /// free. `between` runs after the folder exists and before its lock file
    /// is opened, the moment a cleanup in another process can act on it.
    fn create_named(
        directory: &Path,
        names: impl Iterator<Item = String>,
        between: &dyn Fn(&Path),
    ) -> Result<Self, InstallError> {
        let failed = |what: &str, error: std::io::Error| {
            InstallError::refused(format!("The {what} cannot be created: {error}."))
        };
        std::fs::create_dir_all(directory).map_err(|error| failed("packs folder", error))?;
        let mut last = None;
        for name in names.take(5) {
            let path = directory.join(&name);
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    last = Some(error);
                    continue;
                }
                Err(error) => return Err(failed("working folder", error)),
            }
            between(&path);
            // A cleanup that took the lock first makes the lock step fail;
            // one that deleted the folder makes the open fail. The install
            // then takes a new name.
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(path.join(LOCK_FILE))
                .and_then(|file| file.try_lock().map_err(std::io::Error::from).map(|()| file));
            // A folder this install does not hold the lock of is never
            // deleted here; the cleanup that holds it removes it.
            match lock {
                Ok(file) => {
                    let work = Self {
                        path,
                        lock: Some(file),
                        keep: std::cell::Cell::new(false),
                    };
                    match work.check_alive() {
                        Ok(()) => return Ok(work),
                        Err(error) => last = Some(std::io::Error::other(error.message.clone())),
                    }
                }
                Err(error) => last = Some(error),
            }
        }
        Err(failed(
            "working folder",
            last.unwrap_or_else(|| std::io::Error::other("no folder name was free")),
        ))
    }

    /// Fails when the folder is gone or its lock file is no longer the file
    /// this install holds the lock on.
    fn check_alive(&self) -> Result<(), InstallError> {
        let alive = self
            .lock
            .as_ref()
            .is_some_and(|held| names_held_file(held, &self.path.join(LOCK_FILE)));
        if alive {
            Ok(())
        } else {
            Err(InstallError::refused(
                "The working folder was removed while the pack was installed. Nothing was changed.",
            ))
        }
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        match self.lock.take() {
            Some(held) if !self.keep.get() => delete_holding_lock(&self.path, held, &|| {}),
            _ => {}
        }
    }
}

/// Delete the temporary folders that earlier runs left in the packs folder.
/// A folder whose lock an install holds, in this or another process, is kept.
pub fn clean_leftovers(directory: &Path) {
    let Ok(listing) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in listing.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(TEMPORARY_PREFIX) {
            continue;
        }
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            remove_if_unlocked(directory, &path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// File inside a work folder that names the pack whose earlier version sits in
/// the `old` folder.
const ID_FILE: &str = "id";

/// The pack id the work folder records, when the file holds a valid one.
fn read_recorded_id(folder: &Path) -> Option<String> {
    let path = folder.join(ID_FILE);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > 128 {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let id = text.trim();
    index::valid_id(id).then(|| id.to_owned())
}

/// Delete a leftover work folder, never without holding its lock.
///
/// The cleanup opens `lock`, creating it when missing, and takes it. A
/// folder whose lock is held, or whose lock cannot be opened, is skipped. An
/// install that creates its folder after the listing finds the lock file
/// already taken by the cleanup, fails its own lock step and uses a new name.
/// Every decision below is made after the lock is held.
///
/// A folder with an `old` pack is deleted only when the pack it replaced is
/// in the packs folder; when that pack is missing, `old` is moved back first.
/// A folder whose id record is missing or invalid is kept.
fn remove_if_unlocked(directory: &Path, folder: &Path) {
    remove_if_unlocked_with(directory, folder, &|| {});
}

/// [`remove_if_unlocked`] with a hook that runs after the lock is released
/// and before the lock file and the folder are unlinked.
fn remove_if_unlocked_with(directory: &Path, folder: &Path, released: &dyn Fn()) {
    let Ok(held) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join(LOCK_FILE))
    else {
        return;
    };
    if held.try_lock().is_err() {
        return;
    }
    let old = folder.join(OLD_FOLDER);
    // Held until the folder is deleted, so no install of the same id swaps
    // between the decision and the delete.
    let mut _id_lock = None;
    if std::fs::symlink_metadata(&old).is_ok() {
        let Some(id) = read_recorded_id(folder) else {
            return;
        };
        let Ok(Some(guard)) = lock_id(directory, &id, false) else {
            return;
        };
        let target = directory.join(&id);
        if std::fs::symlink_metadata(&target).is_err() && std::fs::rename(&old, &target).is_err() {
            return;
        }
        _id_lock = Some(guard);
    }
    delete_holding_lock(folder, held, released);
}

/// The lock file that serializes work on pack `id` in the packs folder. It is
/// a dot-file, never deleted, and not a pack folder.
#[must_use]
pub fn id_lock_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!(".{id}.lock"))
}

/// Take the lock of pack `id`. With `wait` the call blocks until the lock is
/// free; without it the result is `None` when another holder has it.
///
/// `id` must pass the pack id rule, so the file name cannot leave the folder.
fn lock_id(directory: &Path, id: &str, wait: bool) -> std::io::Result<Option<std::fs::File>> {
    if !index::valid_id(id) {
        return Err(std::io::Error::other("not a pack identifier"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(id_lock_path(directory, id))?;
    if wait {
        file.lock()?;
        Ok(Some(file))
    } else {
        Ok(file.try_lock().is_ok().then_some(file))
    }
}

/// Delete the content of a work folder while its lock is held, then the lock
/// file and the folder itself.
///
/// This is the only place a work folder is deleted. The recursive deletes run
/// before the lock is released; afterwards only the lock file and the empty
/// folder are removed, each without recursion, so an install that took the
/// lock in between keeps whatever it wrote. A folder that is not empty stays
/// for the next cleanup. `released` runs after the lock is released and the
/// lock file is unlinked.
fn delete_holding_lock(folder: &Path, held: std::fs::File, released: &dyn Fn()) {
    if let Ok(children) = std::fs::read_dir(folder) {
        for child in children.filter_map(Result::ok) {
            if child.file_name() == LOCK_FILE {
                continue;
            }
            let path = child.path();
            let _ = if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
        }
    }
    // The lock file is unlinked only while its lock is held, and only when
    // the path still names the file that is held. On Unix a lock on an
    // unlinked file would protect nothing: a later open of the path makes a
    // new file that anyone can lock. On Windows the unlink of an open file
    // leaves a delete-pending name that cannot be opened until the handle
    // closes.
    let lock_path = folder.join(LOCK_FILE);
    if names_held_file(&held, &lock_path) {
        let _ = std::fs::remove_file(&lock_path);
    }
    drop(held);
    released();
    let _ = std::fs::remove_dir(folder);
}

/// True when `path` names the file `held` is open on.
///
/// Unix compares device and inode. On Windows a name that exists is the held
/// file, because the name of a file that was unlinked while open is
/// delete-pending and cannot be reopened.
fn names_held_file(held: &std::fs::File, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (held.metadata(), std::fs::metadata(path)) {
            (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = held;
        path.is_file()
    }
}

/// Download, check and install the pack of `entry` into `directory`.
///
/// The steps: download with the size of the list as the limit; compare the
/// byte count and the SHA-256; unpack into a temporary folder; load it with
/// the installed-pack loader; compare the manifest id with the list; move the
/// folder to `<directory>/<id>`. A pack of that id the browser installed
/// earlier is replaced; a pack of that id that the browser did not install
/// stops the install. Any failure leaves no new folder.
///
/// # Errors
/// The step that failed and the text to show.
pub fn install(
    services: &PackServices,
    entry: &IndexEntry,
    cancel: &Arc<AtomicBool>,
) -> Result<(), InstallError> {
    let directory = &services.directory;
    let work = Work::create(directory)?;
    let zip = work.path.join("download.zip");
    services
        .transport
        .file(
            &services.locations.file_url(&entry.asset),
            &zip,
            entry.size,
            cancel,
        )
        .map_err(|error| InstallError::download(error.to_string()))?;
    check_download(&zip, entry)?;
    let files = work.path.join("files");
    extract(&zip, &files)?;
    let _ = std::fs::remove_file(&zip);
    let pack = {
        let source = DirectoryFiles::new(&files).map_err(|e| InstallError::refused(e.message()))?;
        let manifest = load::PackFiles::read(&source, MANIFEST_FILE, MAX_MANIFEST_BYTES)
            .map_err(|e| InstallError::refused(e.message()))?
            .ok_or_else(|| InstallError::refused("The pack has no pack.json file."))?;
        load::load(&manifest, &source).map_err(|e| InstallError::refused(e.message()))?
    };
    if pack.id() != entry.id {
        return Err(InstallError::refused(
            "The pack inside the zip has another identifier than the pack list names.",
        ));
    }
    write_file(&files.join(MARKER_FILE), entry.sha256.as_bytes())
        .map_err(|error| InstallError::refused(format!("The pack cannot be marked: {error}.")))?;
    work.check_alive()?;
    move_into_place(directory, &entry.id, &files, &work)
}

fn check_download(zip: &Path, entry: &IndexEntry) -> Result<(), InstallError> {
    let mismatch = || InstallError::refused("The downloaded file does not match the pack list.");
    let mut file = std::fs::File::open(zip)
        .map_err(|error| InstallError::download(format!("the download cannot be read: {error}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            InstallError::download(format!("the download cannot be read: {error}"))
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > entry.size {
            return Err(mismatch());
        }
        hasher.update(&buffer[..read]);
    }
    if total != entry.size || hex(&hasher.finalize()) != entry.sha256 {
        return Err(mismatch());
    }
    Ok(())
}

/// Unpack `zip` into the new folder `destination`.
fn extract(zip: &Path, destination: &Path) -> Result<(), InstallError> {
    let unreadable = |error: &dyn std::fmt::Display| {
        InstallError::refused(format!("The pack is not a usable zip file: {error}."))
    };
    let file = std::fs::File::open(zip).map_err(|error| unreadable(&error))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| unreadable(&error))?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(InstallError::refused(format!(
            "The pack holds more than {MAX_ZIP_ENTRIES} files."
        )));
    }
    std::fs::create_dir(destination).map_err(|error| unreadable(&error))?;
    let allowed_total = (MAX_TOTAL_FILE_BYTES + MAX_MANIFEST_BYTES) as u64;
    let mut total = 0u64;
    let mut seen = BTreeSet::new();
    for position in 0..archive.len() {
        let mut member = archive
            .by_index(position)
            .map_err(|error| unreadable(&error))?;
        let name = member
            .name()
            .map_err(|error| unreadable(&error))?
            .into_owned();
        let regular = member
            .unix_mode()
            .is_none_or(|mode| mode & 0o170_000 == 0o100_000);
        if !member.is_file() || !regular {
            return Err(InstallError::refused(format!(
                "The pack holds \"{}\", which is not a regular file.",
                name.chars()
                    .take(index::MAX_ENTRY_NAME_CHARS)
                    .collect::<String>()
            )));
        }
        check_entry_name(&name).map_err(InstallError::refused)?;
        if !seen.insert(name.clone()) {
            return Err(InstallError::refused(format!(
                "The pack holds \"{name}\" twice."
            )));
        }
        let limit = if name == MANIFEST_FILE {
            MAX_MANIFEST_BYTES
        } else {
            MAX_FILE_BYTES
        } as u64;
        if member.size() > limit {
            return Err(InstallError::refused(format!(
                "The file \"{name}\" is larger than {limit} bytes."
            )));
        }
        let target = name
            .split('/')
            .fold(destination.to_path_buf(), |path, part| path.join(part));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| unreadable(&error))?;
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| unreadable(&error))?;
        let written = std::io::copy(&mut (&mut member).take(limit + 1), &mut out)
            .map_err(|error| unreadable(&error))?;
        if written > limit {
            return Err(InstallError::refused(format!(
                "The file \"{name}\" is larger than {limit} bytes."
            )));
        }
        total += written;
        if total > allowed_total {
            return Err(InstallError::refused(format!(
                "The pack is larger than {allowed_total} bytes unpacked."
            )));
        }
    }
    Ok(())
}

fn move_into_place(
    directory: &Path,
    id: &str,
    files: &Path,
    work: &Work,
) -> Result<(), InstallError> {
    let target = directory.join(id);
    let moved = |error: std::io::Error| {
        InstallError::refused(format!("The pack cannot be put in place: {error}."))
    };
    // Installs of one id in several processes take turns: the target is
    // observed, replaced and, on failure, restored under this lock. The wait
    // runs on the install's worker thread. The lock is taken after the work
    // folder lock and never the other way round while waiting, so no cycle
    // forms.
    let _id_lock = lock_id(directory, id, true).map_err(moved)?;
    match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(files, &target).map_err(moved)
        }
        Err(error) => Err(moved(error)),
        Ok(meta) if meta.is_dir() && read_marker(&target).is_some() => {
            let old = work.path.join(OLD_FOLDER);
            // A crash after the first rename leaves the record the cleanup
            // needs to put the earlier pack back.
            std::fs::write(work.path.join(ID_FILE), id).map_err(moved)?;
            std::fs::rename(&target, &old).map_err(moved)?;
            if let Err(error) = std::fs::rename(files, &target) {
                if std::fs::rename(&old, &target).is_err() {
                    work.keep.set(true);
                    return Err(InstallError::refused(format!(
                        "The pack cannot be put in place: {error}. The earlier version could not be restored; it is in {}.",
                        old.display()
                    )));
                }
                return Err(moved(error));
            }
            Ok(())
        }
        Ok(_) => Err(InstallError::refused(
            "A pack with this identifier is already in the packs folder. It was not installed from the pack list, so it was not changed.",
        )),
    }
}

/// The checksum the browser wrote into the folder of an installed pack, when
/// the folder is a plain folder with a plain marker file.
#[must_use]
pub fn read_marker(folder: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(folder).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let marker = folder.join(MARKER_FILE);
    let meta = std::fs::symlink_metadata(&marker).ok()?;
    if !meta.is_file() || meta.len() > 128 {
        return None;
    }
    let text = std::fs::read_to_string(&marker).ok()?;
    let text = text.trim();
    (text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// The packs the browser installed: folder name and recorded checksum.
#[must_use]
pub fn installed_by_browser(directory: &Path) -> BTreeMap<String, String> {
    installed_folders(directory)
        .into_iter()
        .filter_map(|folder| {
            let name = folder.file_name()?.to_str()?.to_owned();
            let sum = read_marker(&folder)?;
            Some((name, sum))
        })
        .collect()
}

/// Delete the pack `id` when the browser installed it.
///
/// # Errors
/// The text to show when the pack is not one the browser installed or the
/// folder cannot be deleted.
pub fn remove(directory: &Path, id: &str) -> Result<(), String> {
    if !index::valid_id(id) {
        return Err("This is not a pack identifier.".to_owned());
    }
    let folder = directory.join(id);
    if read_marker(&folder).is_none() {
        return Err(
            "This pack was not installed from the pack list, so it is not removed.".to_owned(),
        );
    }
    std::fs::remove_dir_all(&folder)
        .map_err(|error| format!("The pack cannot be removed: {error}."))
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// SHA-256 of `bytes` as lower case hexadecimal.
#[must_use]
pub fn hex_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_work_folder_name_that_is_taken_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join(".incoming-1-1");
        std::fs::create_dir(&taken).unwrap();
        std::fs::write(taken.join("keep"), b"x").unwrap();
        let names = [".incoming-1-1", ".incoming-1-2"].map(str::to_owned);
        let work = Work::create_named(dir.path(), names.into_iter(), &|_| {}).unwrap();
        assert_eq!(work.path, dir.path().join(".incoming-1-2"));
        assert!(
            taken.join("keep").is_file(),
            "the taken folder is untouched"
        );
    }

    #[test]
    fn the_install_takes_the_next_name_when_a_cleanup_locked_the_first_folder() {
        let dir = tempfile::tempdir().unwrap();
        let names = [".incoming-5-1", ".incoming-5-2"].map(str::to_owned);
        let held = std::cell::RefCell::new(None);
        let work = Work::create_named(dir.path(), names.into_iter(), &|folder| {
            // The cleanup lists the new folder, creates its lock file and
            // takes the lock before the install opens it.
            if folder.ends_with(".incoming-5-1") {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(folder.join(LOCK_FILE))
                    .unwrap();
                file.try_lock().unwrap();
                *held.borrow_mut() = Some(file);
            }
        })
        .unwrap();
        assert_eq!(work.path, dir.path().join(".incoming-5-2"));
    }

    #[test]
    fn an_install_that_locks_after_the_cleanup_decided_keeps_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(".incoming-6-1");
        std::fs::create_dir(&folder).unwrap();
        let install_lock = std::cell::RefCell::new(None);
        remove_if_unlocked_with(dir.path(), &folder, &|| {
            // The install opens the lock the cleanup just released, takes
            // it, and moves the user's pack into `old`.
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(folder.join(LOCK_FILE))
                .unwrap();
            file.try_lock().unwrap();
            *install_lock.borrow_mut() = Some(file);
            std::fs::create_dir(folder.join(OLD_FOLDER)).unwrap();
            std::fs::write(folder.join("old/pack.json"), b"user pack").unwrap();
        });
        assert_eq!(
            std::fs::read(folder.join("old/pack.json")).unwrap(),
            b"user pack"
        );
    }

    #[test]
    fn a_work_folder_whose_lock_file_was_unlinked_fails_the_alive_check() {
        let dir = tempfile::tempdir().unwrap();
        let names = [".incoming-7-1".to_owned()];
        let work = Work::create_named(dir.path(), names.into_iter(), &|_| {}).unwrap();
        work.check_alive().unwrap();
        // Another cleanup unlinks the lock file it believed abandoned. On
        // Windows the unlink of an open file may be refused; then the file
        // is still the held one and the check passes.
        let unlinked = std::fs::remove_file(work.path.join(LOCK_FILE)).is_ok();
        if unlinked {
            assert!(work.check_alive().is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_new_lock_file_at_the_same_path_does_not_pass_for_the_held_one() {
        let dir = tempfile::tempdir().unwrap();
        let names = [".incoming-8-1".to_owned()];
        let work = Work::create_named(dir.path(), names.into_iter(), &|_| {}).unwrap();
        // Cleanup A unlinks the lock file; cleanup B then creates a new one at
        // the path, locks it and deletes the children.
        std::fs::create_dir(work.path.join("files")).unwrap();
        std::fs::remove_file(work.path.join(LOCK_FILE)).unwrap();
        remove_if_unlocked(dir.path(), &work.path);
        assert!(!work.path.join("files").exists(), "B held its own lock");
        // The install has not moved anything yet: it fails safe here.
        assert!(work.check_alive().is_err());
    }

    #[test]
    fn five_taken_names_fail_the_install_without_touching_them() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..6).map(|n| format!(".incoming-2-{n}")).collect();
        for name in &names[..5] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        assert!(Work::create_named(dir.path(), names.into_iter(), &|_| {}).is_err());
        assert!(!dir.path().join(".incoming-2-5").exists());
    }
}
