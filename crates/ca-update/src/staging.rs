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
use std::time::{Duration, SystemTime};

const RECORD_FILE: &str = "staged.json";
const WARNING_FILE: &str = "download-warning.json";
const APPLYING_FILE: &str = "applying.lock";
/// Folder inside the staging folder that holds copies of the program used as
/// the apply helper.
pub const HELPER_FOLDER: &str = "helper";
const FORMAT: u32 = 1;
const MAX_RECORD_BYTES: u64 = 16 * 1024;
/// Largest release file the updater stages.
pub const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
/// How long an apply marker blocks a new download.
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
    /// helper folder stay.
    pub fn clear(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == WARNING_FILE || name == HELPER_FOLDER || name == APPLYING_FILE {
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

    /// Delete the warning.
    pub fn clear_warning(&self) {
        let _ = std::fs::remove_file(self.dir.join(WARNING_FILE));
    }

    /// Mark that an apply runs. A download waits while the mark is fresh.
    ///
    /// # Errors
    /// Returns why the mark could not be written.
    pub fn begin_apply(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        std::fs::write(self.dir.join(APPLYING_FILE), b"1").map_err(|error| error.to_string())
    }

    /// Remove the apply mark.
    pub fn end_apply(&self) {
        let _ = std::fs::remove_file(self.dir.join(APPLYING_FILE));
    }

    /// True while an apply runs.
    #[must_use]
    pub fn is_applying(&self) -> bool {
        std::fs::metadata(self.dir.join(APPLYING_FILE))
            .and_then(|meta| meta.modified())
            .is_ok_and(|time| {
                SystemTime::now()
                    .duration_since(time)
                    .map_or(true, |age| age < APPLY_MARKER_AGE)
            })
    }
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
}
