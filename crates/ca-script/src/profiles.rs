//! Turning a remote location written in a script into a file system.
//!
//! The script engine knows nothing about where profiles live. The caller
//! supplies a lookup, and a run with no lookup refuses every remote location
//! with a typed failure rather than reaching the network.
//!
//! A profile that needs a secret this run has no store for is refused by name.
//! Nothing prompts: a script runs unattended, and a prompt on a console is a
//! run that never ends.

use std::sync::Arc;

use ca_vfs::remote::profile::ServiceProfile;
use ca_vfs::{FileSystem, RemoteContext, RemoteProfile, SecretRef, SecretStore};

use crate::error::ExecError;

/// The schemes a remote location is written with.
const SCHEMES: &[&str] = &[
    "ftp://", "ftps://", "sftp://", "http://", "https://", "s3://", "dav://", "davs://",
];

/// True when a location names a remote service rather than a path.
#[must_use]
pub fn is_remote(location: &str) -> bool {
    let lowered = location.to_ascii_lowercase();
    SCHEMES.iter().any(|scheme| lowered.starts_with(scheme))
}

/// Where the profile behind a remote location comes from.
///
/// One implementation reads the settings directory. A test supplies its own,
/// so no test needs the settings directory or the network.
pub trait ProfileLookup: std::fmt::Debug + Send + Sync {
    /// The profile a location names, or `None` when nothing matches.
    fn profile(&self, location: &str) -> Option<RemoteProfile>;

    /// Where secret values come from. The default store holds none, so a
    /// profile that needs one is refused by name.
    fn secrets(&self) -> Arc<dyn SecretStore> {
        Arc::new(NoStore)
    }
}

/// A store that holds nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStore;

impl SecretStore for NoStore {
    fn secret(&self, _reference: &SecretRef) -> Option<ca_vfs::Secret> {
        None
    }
}

/// A lookup that answers nothing, which refuses every remote location.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProfiles;

impl ProfileLookup for NoProfiles {
    fn profile(&self, _location: &str) -> Option<RemoteProfile> {
        None
    }
}

/// Open the file system one remote location names.
///
/// # Errors
/// Returns [`ExecError::NotSupported`] when no profile matches the location or
/// the protocol is not in this build, and [`ExecError::Refused`] when a secret
/// the profile needs is not in the store, or when the connection fails.
pub fn connect(
    location: &str,
    lookup: &dyn ProfileLookup,
    cancel: &ca_vfs::Cancel,
) -> Result<Arc<dyn FileSystem>, ExecError> {
    let profile = lookup.profile(location).ok_or_else(|| {
        ExecError::not_supported("load", format!("no stored profile matches {location}"))
    })?;
    let secrets = lookup.secrets();
    if let Some(missing) = missing_secret(&profile, secrets.as_ref()) {
        return Err(ExecError::Refused(format!(
            "the profile {} needs the secret {missing}, and this run has no store holding it",
            profile.name
        )));
    }
    let context = RemoteContext::with_secrets(secrets);
    ca_vfs::remote::connect(&profile, &context, cancel)
        .map_err(|error| ExecError::Refused(error.to_string()))
}

/// The identifier of the first secret a profile names that the store does not
/// hold.
#[must_use]
pub fn missing_secret(profile: &RemoteProfile, secrets: &dyn SecretStore) -> Option<String> {
    let mut wanted: Vec<&SecretRef> = Vec::new();
    match &profile.service {
        ServiceProfile::Ftp(settings) => {
            wanted.push(&settings.login.password);
            wanted.push(&settings.global.ssh_private_key_passphrase);
        }
        ServiceProfile::WebDav(settings) => wanted.push(&settings.password),
        ServiceProfile::S3(settings) => {
            if let ca_vfs::remote::profile::S3Auth::Saved {
                secret_access_key,
                session_token,
                ..
            } = &settings.auth
            {
                wanted.push(secret_access_key);
                wanted.push(session_token);
            }
        }
        _ => {}
    }
    wanted
        .into_iter()
        .filter(|reference| !reference.is_empty())
        .find(|reference| secrets.secret(reference).is_none())
        .map(|reference| reference.id().to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{connect, is_remote, missing_secret, NoProfiles, NoStore, ProfileLookup};
    use ca_vfs::remote::profile::{ServiceProfile, WebDavProfile};
    use ca_vfs::{RemoteProfile, SecretRef};
    use std::sync::Arc;

    fn needs_a_secret() -> RemoteProfile {
        RemoteProfile {
            name: "shelf".to_owned(),
            service: ServiceProfile::WebDav(WebDavProfile {
                password: SecretRef::new("shelf/password"),
                ..WebDavProfile::default()
            }),
            ..RemoteProfile::default()
        }
    }

    #[derive(Debug)]
    struct OneProfile;

    impl ProfileLookup for OneProfile {
        fn profile(&self, _location: &str) -> Option<RemoteProfile> {
            Some(needs_a_secret())
        }
    }

    #[test]
    fn a_path_is_not_a_remote_location() {
        assert!(!is_remote("C:/folder"));
        assert!(!is_remote("relative/folder"));
        assert!(is_remote("sftp://host/dir"));
        assert!(is_remote("S3://bucket/key"));
    }

    #[test]
    fn a_run_with_no_lookup_refuses_every_remote_location() {
        let error = connect("sftp://host/dir", &NoProfiles, &ca_vfs::Cancel::new())
            .err()
            .unwrap_or_else(|| panic!("a run with no lookup must refuse"));
        assert!(error.is_not_supported(), "{error}");
    }

    #[test]
    fn a_missing_secret_is_named_and_nothing_is_dialled() {
        let error = connect("dav://host/dir", &OneProfile, &ca_vfs::Cancel::new())
            .err()
            .unwrap_or_else(|| panic!("a missing secret must refuse"));
        let text = error.to_string();
        assert!(text.contains("shelf/password"), "{text}");
    }

    #[test]
    fn a_store_holding_the_secret_leaves_nothing_missing() {
        let store = ca_vfs::MemorySecretStore::new();
        store.insert("shelf/password", "value");
        let store: Arc<dyn ca_vfs::SecretStore> = Arc::new(store);
        assert!(missing_secret(&needs_a_secret(), store.as_ref()).is_none());
        assert_eq!(
            missing_secret(&needs_a_secret(), &NoStore).as_deref(),
            Some("shelf/password")
        );
    }
}
