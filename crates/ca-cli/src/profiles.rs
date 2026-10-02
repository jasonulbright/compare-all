//! Remote profiles read from the settings folder.
//!
//! Each profile is one document in the `profiles` folder below the settings
//! folder, named after the profile. A location names a profile by its host
//! part, so `sftp://build-server/out` is the profile `build-server`.
//!
//! Secrets come from the operating system credential store when the
//! `os-keychain` feature is in the build. Nothing is read from a console: a
//! command line run may be silent and unattended, and a prompt there is a run
//! that never ends. A profile whose secret the store does not hold is refused
//! by name.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ca_script::ProfileLookup;
use ca_vfs::remote::profile::MAX_PROFILE_BYTES;
use ca_vfs::{RemoteProfile, SecretStore};

/// Name of the folder profiles live in, below the settings folder.
pub const PROFILE_FOLDER: &str = "profiles";

/// Name the credential store entries of this program sit under.
pub const CREDENTIAL_SERVICE: &str = "compare-all";

/// The extension one stored profile carries.
const PROFILE_EXTENSION: &str = "json";

/// Profiles stored as documents below one folder.
#[derive(Clone)]
pub struct StoredProfiles {
    folder: PathBuf,
    secrets: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for StoredProfiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredProfiles")
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

impl StoredProfiles {
    /// Read profiles from `folder`.
    #[must_use]
    pub fn in_folder(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
            secrets: secret_store(),
        }
    }

    /// Read secrets from `secrets` rather than from the platform store.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Arc<dyn SecretStore>) -> Self {
        self.secrets = secrets;
        self
    }

    /// Read profiles from the settings folder of this run.
    #[must_use]
    pub fn of_settings() -> Self {
        Self::in_folder(ca_script::paths::settings_directory().join(PROFILE_FOLDER))
    }

    /// The folder the profiles are read from.
    #[must_use]
    pub fn folder(&self) -> &Path {
        &self.folder
    }
}

/// The profile name a location addresses: the part between the scheme and the
/// first path separator, with any user name and port removed.
#[must_use]
pub fn profile_name(location: &str) -> Option<String> {
    let rest = location.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    Some(host.to_owned())
}

impl ProfileLookup for StoredProfiles {
    fn profile(&self, location: &str) -> Option<RemoteProfile> {
        let name = profile_name(location)?;
        // A name taken from a location never reaches the file system as a
        // path: a separator or a parent reference in it would leave the
        // profile folder.
        if name.contains(['/', '\\', ':']) || name.contains("..") {
            return None;
        }
        let path = self.folder.join(format!("{name}.{PROFILE_EXTENSION}"));
        let file = std::fs::File::open(path).ok()?;
        if file.metadata().ok()?.len() > MAX_PROFILE_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_PROFILE_BYTES + 1),
            &mut bytes,
        )
        .ok()?;
        if bytes.len() as u64 > MAX_PROFILE_BYTES {
            return None;
        }
        // The program lists a document only under the name it states, so a
        // document stored under another name is not a profile here either.
        let profile: RemoteProfile = serde_json::from_slice(&bytes).ok()?;
        (profile.name.to_lowercase() == name.to_lowercase()).then_some(profile)
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        Arc::clone(&self.secrets)
    }
}

/// The store profile secrets are read from.
#[cfg(feature = "os-keychain")]
#[must_use]
pub fn secret_store() -> Arc<dyn SecretStore> {
    Arc::new(ca_vfs::OsSecretStore::new(CREDENTIAL_SERVICE))
}

/// The store profile secrets are read from.
#[cfg(not(feature = "os-keychain"))]
#[must_use]
pub fn secret_store() -> Arc<dyn SecretStore> {
    Arc::new(ca_script::profiles::NoStore)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{profile_name, StoredProfiles, MAX_PROFILE_BYTES, PROFILE_FOLDER};
    use ca_script::ProfileLookup;

    #[test]
    fn a_location_names_its_profile_by_host() {
        assert_eq!(
            profile_name("sftp://build-server/out").as_deref(),
            Some("build-server")
        );
        assert_eq!(
            profile_name("ftp://user@host:2121/dir").as_deref(),
            Some("host")
        );
        assert_eq!(profile_name("C:/folder"), None);
        assert_eq!(profile_name("sftp:///dir"), None);
    }

    #[test]
    fn a_stored_document_becomes_a_profile() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(PROFILE_FOLDER);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("shelf.json"),
            r#"{"name":"shelf","service":{"kind":"web_dav"}}"#,
        )
        .unwrap();
        let lookup = StoredProfiles::in_folder(&folder);
        let profile = lookup.profile("dav://shelf/files").unwrap();
        assert_eq!(profile.name, "shelf");
        assert!(lookup.profile("dav://absent/files").is_none());
    }

    #[test]
    fn a_document_the_program_would_not_load_is_not_used_here_either() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(PROFILE_FOLDER);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("shelf.json"),
            r#"{"name":"elsewhere","service":{"kind":"web_dav"}}"#,
        )
        .unwrap();
        let padding = " ".repeat(usize::try_from(MAX_PROFILE_BYTES).unwrap());
        std::fs::write(
            folder.join("large.json"),
            format!(r#"{{"name":"large","service":{{"kind":"web_dav"}}}}{padding}"#),
        )
        .unwrap();
        let lookup = StoredProfiles::in_folder(&folder);
        assert!(lookup.profile("dav://shelf/files").is_none());
        assert!(lookup.profile("dav://large/files").is_none());
    }

    #[test]
    fn a_name_that_would_leave_the_folder_matches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let lookup = StoredProfiles::in_folder(dir.path());
        assert!(lookup.profile("dav://../secrets/files").is_none());
    }

    #[test]
    fn a_profile_that_needs_a_secret_is_refused_and_nothing_is_dialled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("shelf.json"),
            r#"{"name":"shelf","service":{"kind":"web_dav","password":"shelf/password"}}"#,
        )
        .unwrap();
        // The test store holds nothing and reaches no credential store of the
        // machine this runs on.
        let lookup = StoredProfiles::in_folder(dir.path())
            .with_secrets(std::sync::Arc::new(ca_vfs::MemorySecretStore::new()));
        let error =
            ca_script::profiles::connect("dav://shelf/files", &lookup, &ca_vfs::Cancel::new())
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
        assert!(error.contains("shelf/password"), "{error}");
    }

    #[test]
    fn a_secret_in_the_test_store_is_found_by_its_reference() {
        let dir = tempfile::tempdir().unwrap();
        let store = ca_vfs::MemorySecretStore::new();
        store.insert("shelf/password", "hunter2");
        let lookup = StoredProfiles::in_folder(dir.path())
            .with_secrets(std::sync::Arc::new(store))
            .secrets();
        let found = lookup
            .secret(&ca_vfs::SecretRef::new("shelf/password"))
            .unwrap();
        assert_eq!(found.expose(), "hunter2");
    }
}
