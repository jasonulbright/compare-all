//! The staging folder: the verified update and the download warning.
//!
//! Both records survive an exit and a reboot, so both are read as untrusted
//! input. A record that does not pass its structural checks is deleted. A
//! record that passes them still proves nothing about the file next to it:
//! [`crate::verify`] checks the file again before anything is applied.

use crate::manifest::{is_plain_file_name, is_sha256, msi_name, zip_name};
use crate::version::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::time::{Duration, SystemTime};

const RECORD_FILE: &str = "staged.json";
const WARNING_FILE: &str = "download-warning.json";
const APPLYING_FILE: &str = "applying.lock";
const APPLY_FAILURE_FILE: &str = "apply-failure.txt";
const MAX_FAILURE_TEXT: usize = 4096;
/// Folder inside the staging folder that holds copies of the program used as
/// the apply helper.
pub const HELPER_FOLDER: &str = "helper";
const FORMAT: u32 = 1;
const MAX_RECORD_BYTES: u64 = 16 * 1024;
/// Largest release file the updater stages.
pub const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
/// How long a claim file blocks a new claim where no handle can hold it.
#[cfg(not(windows))]
const APPLY_MARKER_AGE: Duration = Duration::from_secs(60 * 60);

/// Which file of a release is staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// The installer.
    Msi,
    /// The portable archive.
    Zip,
}

impl AssetKind {
    /// The name of this kind of file in the release of `version`.
    #[must_use]
    pub fn file_name(self, version: &Version) -> String {
        match self {
            Self::Msi => msi_name(&version.to_string()),
            Self::Zip => zip_name(&version.to_string()),
        }
    }
}

/// The record of one staged update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedRecord {
    /// Layout number of this record.
    pub format: u32,
    /// Version of the staged release.
    pub version: String,
    /// File name of the staged file.
    pub asset: String,
    /// SHA-256 of the staged file.
    pub sha256: String,
    /// Size of the staged file in bytes.
    pub size: u64,
    /// True once the download passed every check.
    pub verified: bool,
}

impl StagedRecord {
    /// A verified record.
    #[must_use]
    pub fn new(version: &Version, asset: String, sha256: String, size: u64) -> Self {
        Self {
            format: FORMAT,
            version: version.to_string(),
            asset,
            sha256,
            size,
            verified: true,
        }
    }

    /// The version, when the record is consistent.
    #[must_use]
    pub fn parsed_version(&self) -> Option<Version> {
        Version::parse(&self.version)
    }

    /// Which kind of file the record names.
    #[must_use]
    pub fn kind(&self) -> Option<AssetKind> {
        let version = self.parsed_version()?;
        [AssetKind::Msi, AssetKind::Zip]
            .into_iter()
            .find(|kind| kind.file_name(&version) == self.asset)
    }

    fn is_consistent(&self) -> bool {
        self.format == FORMAT
            && self.verified
            && self
                .parsed_version()
                .is_some_and(|v| v.to_string() == self.version)
            && is_plain_file_name(&self.asset)
            && self.kind().is_some()
            && is_sha256(&self.sha256)
            && self.size > 0
            && self.size <= MAX_ASSET_BYTES
    }
}

/// The note that a release could not be downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadWarning {
    /// Layout number of this record.
    pub format: u32,
    /// The version that could not be downloaded.
    pub version: String,
    /// The release page of that version.
    pub page: String,
}

impl DownloadWarning {
    /// A warning for `version` whose release page is `page`.
    #[must_use]
    pub fn new(version: &Version, page: String) -> Self {
        Self {
            format: FORMAT,
            version: version.to_string(),
            page,
        }
    }

    /// The version the warning names.
    #[must_use]
    pub fn parsed_version(&self) -> Option<Version> {
        Version::parse(&self.version)
    }
}

/// The staging folder.
#[derive(Debug, Clone)]
pub struct Staging {
    dir: PathBuf,
}

impl Staging {
    /// The staging folder at `dir`. Nothing is created until a write.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the file named by `record` is kept.
    #[must_use]
    pub fn asset_path(&self, record: &StagedRecord) -> PathBuf {
        self.dir.join(&record.asset)
    }

    /// Where a download is written before it is verified.
    #[must_use]
    pub fn partial_path(&self, asset: &str) -> PathBuf {
        self.dir.join(format!("{asset}.part"))
    }

    /// The staged record, if one exists and is structurally sound. An unsound
    /// record is deleted together with the staged files.
    #[must_use]
    pub fn read_record(&self) -> Option<StagedRecord> {
        let record: StagedRecord = read_json(&self.dir.join(RECORD_FILE))?;
        if record.is_consistent() {
            Some(record)
        } else {
            self.clear();
            None
        }
    }

    /// Keep `record`.
    ///
    /// # Errors
    /// Returns why the record could not be written.
    pub fn write_record(&self, record: &StagedRecord) -> Result<(), String> {
        write_json(&self.dir.join(RECORD_FILE), record)
    }

    /// Remove the staged record and every staged file. The warning and the
    /// helper folder stay. While a claim is held, by this process or another,
    /// nothing is removed: the holder still needs the files. The removal holds
    /// a claim of its own, so no claim can start while files are removed.
    pub fn clear(&self) {
        if !self.dir.is_dir() {
            return;
        }
        if let Ok(claim) = self.begin_apply() {
            self.remove_staged();
            drop(claim);
        }
    }

    /// [`Staging::clear`] for the holder of `claim`.
    pub fn clear_claimed(&self, claim: &ApplyClaim) {
        let _ = claim;
        self.remove_staged();
    }

    /// A scratch folder that belongs to this process only.
    #[must_use]
    pub fn scratch_dir(&self) -> PathBuf {
        self.dir.join(format!("scratch-{}", std::process::id()))
    }

    fn remove_staged(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == WARNING_FILE
                || name == HELPER_FOLDER
                || name == APPLYING_FILE
                || name == APPLY_FAILURE_FILE
            {
                continue;
            }
            let path = entry.path();
            let _ = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
        }
    }

    /// Remove the copies of the program used as apply helper. Files still in
    /// use stay and are removed by a later call.
    pub fn sweep_helpers(&self) {
        let Ok(entries) = std::fs::read_dir(self.dir.join(HELPER_FOLDER)) else {
            return;
        };
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
        let _ = std::fs::remove_dir(self.dir.join(HELPER_FOLDER));
    }

    /// The folder for the helper copy, created if needed.
    ///
    /// # Errors
    /// Returns why the folder could not be created.
    pub fn helper_dir(&self) -> Result<PathBuf, String> {
        let path = self.dir.join(HELPER_FOLDER);
        std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        Ok(path)
    }

    /// The warning about a release that could not be downloaded.
    #[must_use]
    pub fn read_warning(&self) -> Option<DownloadWarning> {
        let warning: DownloadWarning = read_json(&self.dir.join(WARNING_FILE))?;
        if warning.format == FORMAT
            && warning.parsed_version().is_some()
            && (warning.page.starts_with("https://") || warning.page.starts_with("http://"))
        {
            Some(warning)
        } else {
            self.clear_warning();
            None
        }
    }

    /// Keep `warning`.
    ///
    /// # Errors
    /// Returns why the warning could not be written.
    pub fn write_warning(&self, warning: &DownloadWarning) -> Result<(), String> {
        write_json(&self.dir.join(WARNING_FILE), warning)
    }

    /// Keep the reason the last apply failed, one line, replacing any earlier
    /// one.
    pub fn write_apply_failure(&self, reason: &str) {
        let line: String = reason
            .replace(['\r', '\n'], " ")
            .chars()
            .take(MAX_FAILURE_TEXT)
            .collect();
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = ca_io::write_atomic(
            &self.dir.join(APPLY_FAILURE_FILE),
            format!("{line}\n").as_bytes(),
        );
    }

    /// The reason the last apply failed, if one did.
    #[must_use]
    pub fn read_apply_failure(&self) -> Option<String> {
        let path = self.dir.join(APPLY_FAILURE_FILE);
        if std::fs::metadata(&path).ok()?.len() > MAX_RECORD_BYTES {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?;
        Some(text.trim().chars().take(MAX_FAILURE_TEXT).collect())
    }

    /// Delete the warning and the reason of a failed apply that goes with it.
    pub fn clear_warning(&self) {
        let _ = std::fs::remove_file(self.dir.join(WARNING_FILE));
        let _ = std::fs::remove_file(self.dir.join(APPLY_FAILURE_FILE));
    }

    /// Claim the staging folder for one download or one apply. A second claim
    /// fails while the first is held.
    ///
    /// On Windows the claim is a handle that shares nothing; the system closes
    /// it when the process ends in any way, so a crashed apply never blocks
    /// the next one. Elsewhere the claim is a file created only when absent; a
    /// file older than an hour is taken over.
    ///
    /// # Errors
    /// Returns why the update could not be claimed.
    pub fn begin_apply(&self) -> Result<ApplyClaim, String> {
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let path = self.dir.join(APPLYING_FILE);
        let file = open_claim(&path).or_else(|error| {
            if !cfg!(windows) && !self.is_applying() {
                let _ = std::fs::remove_file(&path);
                open_claim(&path)
            } else {
                Err(error)
            }
        });
        match file {
            Ok(mut file) => {
                use std::io::Write;
                let _ = file.write_all(b"1");
                Ok(ApplyClaim {
                    file: Some(file),
                    path,
                })
            }
            Err(error) => Err(format!("another update is being applied ({error})")),
        }
    }

    /// True while a claim is held, by this process or another.
    #[must_use]
    pub fn is_applying(&self) -> bool {
        claim_is_held(&self.dir.join(APPLYING_FILE))
    }
}

/// On Windows a held claim is a handle that shares nothing, so any other open
/// fails with a sharing violation; a file left by a process that ended opens
/// and counts as free whatever its time.
#[cfg(windows)]
fn claim_is_held(path: &Path) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => error.raw_os_error() == Some(ERROR_SHARING_VIOLATION),
    }
}

/// Elsewhere a claim file younger than [`APPLY_MARKER_AGE`] is held. A time
/// in the future comes from a changed clock or a planted file and is stale.
#[cfg(not(windows))]
fn claim_is_held(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|time| {
            SystemTime::now()
                .duration_since(time)
                .is_ok_and(|age| age < APPLY_MARKER_AGE)
        })
}

/// The exclusive right to apply the staged update. Dropping it releases the
/// claim.
#[derive(Debug)]
pub struct ApplyClaim {
    file: Option<std::fs::File>,
    path: PathBuf,
}

impl Drop for ApplyClaim {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn open_claim(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn open_claim(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_RECORD_BYTES {
        let _ = std::fs::remove_file(path);
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let parsed = serde_json::from_slice(&bytes).ok();
    if parsed.is_none() {
        let _ = std::fs::remove_file(path);
    }
    parsed
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    ca_io::write_atomic(path, &bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn version() -> Version {
        Version::parse("2026.10.18.0007").unwrap()
    }

    #[test]
    fn a_record_round_trips_and_a_tampered_one_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let record = StagedRecord::new(
            &version(),
            AssetKind::Msi.file_name(&version()),
            "b".repeat(64),
            10,
        );
        staging.write_record(&record).unwrap();
        assert_eq!(staging.read_record(), Some(record.clone()));

        for tamper in [
            |r: &mut StagedRecord| r.asset = "..\\evil.msi".to_owned(),
            |r: &mut StagedRecord| r.asset = "other.msi".to_owned(),
            |r: &mut StagedRecord| r.sha256 = "xyz".to_owned(),
            |r: &mut StagedRecord| r.verified = false,
            |r: &mut StagedRecord| r.format = 9,
            |r: &mut StagedRecord| r.version = "latest".to_owned(),
            |r: &mut StagedRecord| r.size = 0,
        ] {
            let mut bad = record.clone();
            tamper(&mut bad);
            staging.write_record(&bad).unwrap();
            assert_eq!(staging.read_record(), None);
            assert!(!dir.path().join(RECORD_FILE).exists());
        }
        std::fs::write(dir.path().join(RECORD_FILE), b"not json").unwrap();
        assert_eq!(staging.read_record(), None);
    }

    #[test]
    fn an_apply_claim_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let claim = staging.begin_apply().unwrap();
        assert!(staging.is_applying());
        assert!(staging.begin_apply().is_err());
        staging.clear();
        assert!(staging.begin_apply().is_err());
        drop(claim);
        assert!(!staging.is_applying());
        let again = staging.begin_apply().unwrap();
        drop(again);
    }

    #[test]
    fn clearing_keeps_the_warning() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        staging
            .write_warning(&DownloadWarning::new(
                &version(),
                "https://example.test/r".to_owned(),
            ))
            .unwrap();
        std::fs::write(dir.path().join("x.part"), b"1").unwrap();
        staging.clear();
        assert!(staging.read_warning().is_some());
        assert!(!dir.path().join("x.part").exists());
    }

    fn staged(staging: &Staging) -> StagedRecord {
        let record = StagedRecord::new(
            &version(),
            AssetKind::Zip.file_name(&version()),
            "b".repeat(64),
            10,
        );
        std::fs::create_dir_all(staging.dir()).unwrap();
        std::fs::write(staging.asset_path(&record), b"0123456789").unwrap();
        staging.write_record(&record).unwrap();
        record
    }

    #[test]
    fn a_clear_while_the_update_is_claimed_keeps_the_staged_files() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let record = staged(&staging);
        let claim = staging.begin_apply().unwrap();
        staging.clear();
        assert_eq!(staging.read_record(), Some(record.clone()));
        assert!(staging.asset_path(&record).exists());
        drop(claim);
        staging.clear();
        assert_eq!(staging.read_record(), None);
    }

    #[test]
    fn no_claim_starts_while_a_clear_removes_the_staged_files() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        staged(&staging);
        for index in 0..3000 {
            std::fs::write(dir.path().join(format!("{index}.part")), b"1").unwrap();
        }
        let count = |dir: &Path| std::fs::read_dir(dir).unwrap().count();
        let total = count(dir.path());
        let clearing = {
            let staging = staging.clone();
            std::thread::spawn(move || staging.clear())
        };
        while count(dir.path()) == total && !clearing.is_finished() {
            std::thread::yield_now();
        }
        let claim = loop {
            if let Ok(claim) = staging.begin_apply() {
                break claim;
            }
            std::thread::yield_now();
        };
        let held = count(dir.path());
        clearing.join().unwrap();
        assert_eq!(count(dir.path()), held, "files were removed under a claim");
        drop(claim);
    }

    #[test]
    fn a_claim_file_with_a_future_time_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let path = dir.path().join(APPLYING_FILE);
        let file = std::fs::File::create(&path).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(24 * 60 * 60))
            .unwrap();
        drop(file);
        assert!(!staging.is_applying());
        let claim = staging.begin_apply().unwrap();
        assert!(staging.is_applying());
        drop(claim);
    }
}
