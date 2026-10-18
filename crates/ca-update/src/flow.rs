//! The check and the download.
//!
//! The check (latest tag, manifest) and the download are separate steps with
//! separate failures. Many networks pass a JSON read and block, scan or
//! replace a binary from the same host, so a check that succeeds followed by
//! a download that fails is an expected result, reported as
//! [`Outcome::DownloadFailed`], and kept as a warning for the next launch.

use crate::manifest::{Manifest, MANIFEST_NAME};
use crate::mode::InstallMode;
use crate::net::{is_file_content_type, Transport};
use crate::signature::SignatureVerifier;
use crate::staging::{AssetKind, DownloadWarning, StagedRecord, Staging, MAX_ASSET_BYTES};
use crate::verify::{verify_file, Expected};
use crate::version::Version;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Protocol number of this updater. A manifest may demand a higher one.
pub const UPDATER_VERSION: u32 = 1;

const MAX_DOCUMENT_BYTES: u64 = 1 << 20;

/// Where the releases are.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// The document that names the newest release.
    pub latest: String,
    /// The folder of release downloads; a release adds `/<tag>/<file>`.
    pub download_base: String,
    /// The folder of release pages; a release adds `/<tag>`.
    pub page_base: String,
}

impl Endpoints {
    /// The public releases of the project.
    #[must_use]
    pub fn github() -> Self {
        Self {
            latest: "https://api.github.com/repos/jasonulbright/compare-all/releases/latest"
                .to_owned(),
            download_base: "https://github.com/jasonulbright/compare-all/releases/download"
                .to_owned(),
            page_base: "https://github.com/jasonulbright/compare-all/releases/tag".to_owned(),
        }
    }

    /// The page of the release of `version`.
    #[must_use]
    pub fn page(&self, version: &Version) -> String {
        format!("{}/v{version}", self.page_base)
    }
}

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The running build is the newest release or newer.
    Current,
    /// A newer release exists and this build cannot download it by itself.
    Newer {
        /// Its version.
        version: Version,
        /// Its release page.
        page: String,
    },
    /// A newer release is downloaded, verified and staged.
    Staged {
        /// Its version.
        version: Version,
    },
    /// A newer release exists and the download failed.
    DownloadFailed {
        /// Its version.
        version: Version,
        /// Its release page.
        page: String,
        /// Why.
        reason: String,
    },
}

/// Everything one check needs.
pub struct Job<'a> {
    /// The network.
    pub transport: &'a dyn Transport,
    /// The Authenticode verifier.
    pub signer: &'a dyn SignatureVerifier,
    /// The staging folder.
    pub staging: &'a Staging,
    /// How the running program was installed. None turns the download off.
    pub mode: Option<InstallMode>,
    /// The running build.
    pub current: Version,
    /// The release locations.
    pub endpoints: &'a Endpoints,
    /// Common name of the required signer.
    pub signer_name: &'a str,
    /// Set to stop the work.
    pub cancel: &'a Arc<AtomicBool>,
}

/// The version a GitHub release document names.
///
/// # Errors
/// Returns why the document is not a release of this application.
pub fn parse_release(body: &[u8]) -> Result<Version, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| format!("the reply is not JSON: {error}"))?;
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "the reply names no tag".to_owned())?;
    Version::parse(tag).ok_or_else(|| format!("the tag {tag:?} is not a version"))
}

/// Ask for the newest release; when it is newer, download and stage it.
///
/// # Errors
/// Returns why the check itself failed (the latest tag or the manifest could
/// not be read). A failed download is an [`Outcome`], not an error.
pub fn run(job: &Job<'_>) -> Result<Outcome, String> {
    let body = job
        .transport
        .get_document(&job.endpoints.latest, MAX_DOCUMENT_BYTES, job.cancel)?;
    let latest = parse_release(&body)?;
    if latest <= job.current {
        if let Some(warning) = job.staging.read_warning() {
            if warning.parsed_version().is_none_or(|v| v <= job.current) {
                job.staging.clear_warning();
            }
        }
        if job
            .staging
            .read_record()
            .and_then(|record| record.parsed_version())
            .is_none_or(|v| v <= job.current)
        {
            job.staging.clear();
        }
        return Ok(Outcome::Current);
    }
    let page = job.endpoints.page(&latest);
    let Some(mode) = job.mode else {
        return Ok(Outcome::Newer {
            version: latest,
            page,
        });
    };
    let manifest_url = format!("{}/v{latest}/{MANIFEST_NAME}", job.endpoints.download_base);
    let manifest = Manifest::parse(&job.transport.get_document(
        &manifest_url,
        MAX_DOCUMENT_BYTES,
        job.cancel,
    )?)?;
    if Version::parse(&manifest.version) != Some(latest) {
        return Err("the manifest belongs to another release".to_owned());
    }
    if job.staging.is_applying() {
        return Ok(Outcome::Current);
    }
    match stage(job, mode, &latest, &manifest) {
        Ok(()) => {
            job.staging.clear_warning();
            Ok(Outcome::Staged { version: latest })
        }
        Err(reason) => {
            job.staging.clear();
            let _ = job
                .staging
                .write_warning(&DownloadWarning::new(&latest, page.clone()));
            Ok(Outcome::DownloadFailed {
                version: latest,
                page,
                reason,
            })
        }
    }
}

fn stage(
    job: &Job<'_>,
    mode: InstallMode,
    latest: &Version,
    manifest: &Manifest,
) -> Result<(), String> {
    if manifest.minimum_updater_version > UPDATER_VERSION {
        return Err("this release needs a newer updater than this build has".to_owned());
    }
    let kind = match mode {
        InstallMode::Installed { .. } => AssetKind::Msi,
        InstallMode::Portable => AssetKind::Zip,
    };
    let name = kind.file_name(latest);
    let asset = manifest
        .asset(&name)
        .ok_or_else(|| format!("the manifest lists no {name}"))?;
    if asset.size == 0 || asset.size > MAX_ASSET_BYTES {
        return Err(format!("the manifest lists an unusable size for {name}"));
    }
    let expected = Expected {
        kind,
        size: asset.size,
        sha256: &asset.sha256,
        signer: job.signer_name,
    };
    let record = StagedRecord::new(latest, name.clone(), asset.sha256.clone(), asset.size);
    let scratch = job.staging.dir().join("scratch");
    let _ = std::fs::remove_dir_all(&scratch);

    if job.staging.read_record().as_ref() == Some(&record) {
        let kept = job.staging.asset_path(&record);
        let result = verify_file(&kept, &expected, job.signer, &scratch);
        let _ = std::fs::remove_dir_all(&scratch);
        if result.is_ok() {
            return Ok(());
        }
    }
    job.staging.clear();
    std::fs::create_dir_all(job.staging.dir()).map_err(|error| error.to_string())?;
    let partial = job.staging.partial_path(&name);
    let url = format!("{}/v{latest}/{name}", job.endpoints.download_base);
    let outcome = (|| {
        let fetched = job
            .transport
            .download(&url, &partial, asset.size.max(1), job.cancel)?;
        if !is_file_content_type(fetched.content_type.as_deref()) {
            return Err(format!(
                "the server answered with {}, not a file",
                fetched.content_type.as_deref().unwrap_or("no type")
            ));
        }
        verify_file(&partial, &expected, job.signer, &scratch)?;
        std::fs::rename(&partial, job.staging.asset_path(&record))
            .map_err(|error| format!("cannot keep the download: {error}"))?;
        job.staging.write_record(&record)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_file(&partial);
    if outcome.is_err() {
        job.staging.clear();
    }
    outcome
}
