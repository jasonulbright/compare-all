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
use std::sync::{Arc, Mutex};

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

static ACTIVE: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary folder inside the packs folder. Dropping it deletes it, so a
/// failed install leaves nothing behind.
struct Work {
    path: PathBuf,
    name: String,
}

impl Work {
    fn create(directory: &Path) -> Result<Self, InstallError> {
        std::fs::create_dir_all(directory).map_err(|error| {
            InstallError::refused(format!("The packs folder cannot be created: {error}."))
        })?;
        let name = format!(
            "{TEMPORARY_PREFIX}{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = directory.join(&name);
        std::fs::create_dir(&path).map_err(|error| {
            InstallError::refused(format!("The working folder cannot be created: {error}."))
        })?;
        if let Ok(mut active) = ACTIVE.lock() {
            active.insert(name.clone());
        }
        Ok(Self { path, name })
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        if let Ok(mut active) = ACTIVE.lock() {
            active.remove(&self.name);
        }
    }
}

/// Delete the temporary folders that earlier runs left in the packs folder.
/// A folder an install of this process still uses is kept.
pub fn clean_leftovers(directory: &Path) {
    let Ok(listing) = std::fs::read_dir(directory) else {
        return;
    };
    let active = ACTIVE.lock().map(|set| set.clone()).unwrap_or_default();
    for entry in listing.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(TEMPORARY_PREFIX) || active.contains(&name) {
            continue;
        }
        let path = entry.path();
        let is_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
        let _ = if is_folder {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
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
    move_into_place(directory, &entry.id, &files, &work.path)
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
    work: &Path,
) -> Result<(), InstallError> {
    let target = directory.join(id);
    let moved = |error: std::io::Error| {
        InstallError::refused(format!("The pack cannot be put in place: {error}."))
    };
    match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(files, &target).map_err(moved)
        }
        Err(error) => Err(moved(error)),
        Ok(meta) if meta.is_dir() && read_marker(&target).is_some() => {
            let old = work.join("old");
            std::fs::rename(&target, &old).map_err(moved)?;
            if let Err(error) = std::fs::rename(files, &target) {
                let _ = std::fs::rename(&old, &target);
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
