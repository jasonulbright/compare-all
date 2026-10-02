//! Remote file systems: file transfer, secure shell transfer, HTTP shares and
//! object stores, each behind the same [`FileSystem`](crate::FileSystem)
//! trait.
//!
//! # What a server says is not evidence
//!
//! A listing is written by the server. Every name it contains passes through
//! [`VfsPath`] validation before it reaches a path, a tree or a local file, so
//! a name holding `..`, a leading separator, a volume prefix, a NUL or a
//! control character is reported on the entry rather than resolved. A name
//! that cannot be used at all is listed with an error instead of being
//! dropped, which is the same rule the container readers follow.
//!
//! # Bounds
//!
//! Every call has a deadline and polls the caller's
//! [`Cancel`](crate::Cancel). Listings and reads are charged against
//! [`Limits`](crate::Limits), so a server that answers a listing request with
//! an endless stream costs the ceiling rather than the caller's memory.
//!
//! # Secrets
//!
//! Nothing in a [`profile::RemoteProfile`] holds a password. A profile names a
//! [`SecretRef`] and the caller supplies a [`SecretStore`]. [`Secret`] prints
//! as `***`, so a value cannot reach a log line or an error message by
//! accident.

pub mod hostkeys;
pub mod net;
pub mod profile;
pub mod secret;
#[cfg(feature = "subversion")]
pub mod subversion;
pub mod timestamp;
#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "ftp")]
pub mod ftp;
#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "s3")]
pub mod s3;
#[cfg(feature = "sftp")]
pub mod sftp;
#[cfg(feature = "webdav")]
pub mod webdav;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

pub use hostkeys::{KnownHostEntry, KnownHosts};
pub use net::{AddressPreference, Deadline};
pub use profile::{RemoteProfile, ServiceProfile};
#[cfg(feature = "os-keychain")]
pub use secret::OsSecretStore;
pub use secret::{MemorySecretStore, NoSecrets, Secret, SecretRef, SecretStore};
#[cfg(feature = "tls")]
pub use tls::{TlsOptions, TlsVersion};

use crate::error::{VfsError, VfsResult};
use crate::limits::Limits;
use crate::path::VfsPath;

/// Keep displayed remote components off local platform aliases. The caller
/// reports a changed spelling before a local operation can use the row.
#[cfg(any(
    feature = "ftp",
    feature = "sftp",
    feature = "webdav",
    feature = "s3",
    feature = "subversion"
))]
fn local_display_name(name: String) -> String {
    match crate::stored::platform_refusal(std::ffi::OsStr::new(&name)) {
        Some((display, _)) => display,
        None => name,
    }
}

/// Settings a connection needs that do not belong to any one profile.
#[derive(Clone)]
pub struct RemoteContext {
    /// Where secret values come from.
    pub secrets: Arc<dyn SecretStore>,
    /// Ceilings on what one listing or one read may cost.
    pub limits: Limits,
    /// How long one call may take before it fails.
    pub call_timeout: Duration,
    /// Where accepted secure shell host keys are recorded. A profile that
    /// names its own store overrides this.
    pub known_hosts: Option<KnownHosts>,
}

impl std::fmt::Debug for RemoteContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteContext")
            .field("limits", &self.limits)
            .field("call_timeout", &self.call_timeout)
            .field("known_hosts", &self.known_hosts)
            .finish_non_exhaustive()
    }
}

impl Default for RemoteContext {
    fn default() -> Self {
        Self {
            secrets: Arc::new(NoSecrets),
            limits: Limits::default(),
            call_timeout: Duration::from_secs(60),
            known_hosts: None,
        }
    }
}

impl RemoteContext {
    /// A context backed by `secrets`.
    #[must_use]
    pub fn with_secrets(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            secrets,
            ..Self::default()
        }
    }

    /// A deadline one call from now.
    #[must_use]
    pub fn deadline(&self) -> Deadline {
        Deadline::after(self.call_timeout)
    }

    /// The secret behind `reference`, or `None` when the reference is empty or
    /// the store holds nothing.
    #[must_use]
    pub fn secret(&self, reference: &SecretRef) -> Option<Secret> {
        if reference.is_empty() {
            return None;
        }
        self.secrets.secret(reference)
    }
}

/// Open the file system a profile describes.
///
/// # Errors
/// Returns [`VfsError::Unsupported`] for a service this build does not
/// connect to, and whatever the protocol reports for one it does.
pub fn connect(
    profile: &RemoteProfile,
    context: &RemoteContext,
    cancel: &crate::cancel::Cancel,
) -> VfsResult<Arc<dyn crate::fs::FileSystem>> {
    // A build with every protocol feature dropped uses neither.
    let _ = (context, cancel);
    match &profile.service {
        #[cfg(any(feature = "ftp", feature = "sftp"))]
        ServiceProfile::Ftp(settings) => connect_transfer(settings, context, cancel),
        #[cfg(not(any(feature = "ftp", feature = "sftp")))]
        ServiceProfile::Ftp(_) => Err(VfsError::unsupported(
            "this build was compiled without the file transfer protocols",
        )),
        #[cfg(feature = "webdav")]
        ServiceProfile::WebDav(settings) => Ok(Arc::new(webdav::WebDavFs::connect(
            settings, context, cancel,
        )?)),
        #[cfg(not(feature = "webdav"))]
        ServiceProfile::WebDav(_) => Err(VfsError::unsupported(
            "this build was compiled without the HTTP share protocol",
        )),
        #[cfg(feature = "s3")]
        ServiceProfile::S3(settings) => Ok(Arc::new(s3::S3Fs::connect(settings, context, cancel)?)),
        #[cfg(not(feature = "s3"))]
        ServiceProfile::S3(_) => Err(VfsError::unsupported(
            "this build was compiled without the object store protocol",
        )),
        ServiceProfile::Dropbox(_) => Err(VfsError::unsupported(UNSUPPORTED_DROPBOX)),
        ServiceProfile::OneDrive(_) => Err(VfsError::unsupported(UNSUPPORTED_ONEDRIVE)),
        #[cfg(feature = "subversion")]
        ServiceProfile::Subversion(settings) => Ok(Arc::new(subversion::SubversionFs::connect(
            settings, context, cancel,
        )?)),
        #[cfg(not(feature = "subversion"))]
        ServiceProfile::Subversion(_) => Err(VfsError::unsupported(UNSUPPORTED_SUBVERSION)),
        ServiceProfile::Unknown(_) => Err(VfsError::unsupported(
            "the profile names a service this build does not know",
        )),
    }
}

/// Why a Dropbox profile does not connect.
pub const UNSUPPORTED_DROPBOX: &str = "Dropbox needs an application registration with the service \
                                       before any account can authorize this program; the profile \
                                       type is defined and the connector is not";

/// Why a `OneDrive` profile does not connect.
pub const UNSUPPORTED_ONEDRIVE: &str = "OneDrive needs an application registration with the \
                                        service before any account can authorize this program; \
                                        the profile type is defined and the connector is not";

/// Why a Subversion profile does not connect when this build omits its
/// connector.
pub const UNSUPPORTED_SUBVERSION: &str = "Subversion access needs the revision control command \
                                          line tools on the search path; the profile type is \
                                          defined and the connector is not";

/// Open a file transfer profile with the protocol its login names.
#[cfg(any(feature = "ftp", feature = "sftp"))]
fn connect_transfer(
    settings: &profile::FtpProfile,
    context: &RemoteContext,
    cancel: &crate::cancel::Cancel,
) -> VfsResult<Arc<dyn crate::fs::FileSystem>> {
    match settings.login.protocol {
        #[cfg(feature = "sftp")]
        profile::FtpProtocol::Sftp => {
            Ok(Arc::new(sftp::SftpFs::connect(settings, context, cancel)?))
        }
        #[cfg(not(feature = "sftp"))]
        profile::FtpProtocol::Sftp => Err(VfsError::unsupported(
            "this build was compiled without the secure shell transfer protocol",
        )),
        #[cfg(feature = "ftp")]
        profile::FtpProtocol::Ftp
        | profile::FtpProtocol::FtpsExplicit
        | profile::FtpProtocol::FtpsImplicit => {
            Ok(Arc::new(ftp::FtpFs::connect(settings, context, cancel)?))
        }
        #[cfg(not(feature = "ftp"))]
        profile::FtpProtocol::Ftp
        | profile::FtpProtocol::FtpsExplicit
        | profile::FtpProtocol::FtpsImplicit => Err(VfsError::unsupported(
            "this build was compiled without the file transfer protocol",
        )),
        profile::FtpProtocol::Unknown(_) => Err(VfsError::unsupported(
            "the profile names a transfer protocol this build does not know",
        )),
    }
}

/// Turn one server-supplied name into a child path of `parent`.
///
/// The name is a claim the server makes. A separator, a parent component, a
/// volume prefix, a NUL or a control character in it means the entry cannot be
/// placed in the tree, and the caller reports the entry with the reason rather
/// than resolving it.
///
/// # Errors
/// Returns [`VfsError::InvalidPath`] for any name that is not a single usable
/// component, and [`VfsError::Protocol`] for the two navigation names.
pub fn child_path(parent: &VfsPath, name: &str) -> VfsResult<VfsPath> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(VfsError::protocol(format!(
            "the listing names an entry {name:?}, which is not a child"
        )));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(VfsError::protocol(format!(
            "the listing names an entry {name:?}, which is a path rather than a child"
        )));
    }
    Ok(parent.join(name)?)
}

/// The names already placed in one listing, compared without letter case.
#[derive(Debug, Default)]
pub struct ListedNames {
    taken: BTreeSet<String>,
    /// The next suffix to try for each folded name, so a name the server
    /// repeats many times does not rescan every suffix it already used.
    next: BTreeMap<String, u64>,
}

impl ListedNames {
    /// Claim `name`, or the first `name~N` no placed name folds to when one
    /// already does. The flag is true when the name was changed.
    #[must_use]
    pub fn claim(&mut self, name: String) -> (String, bool) {
        let folded = name.to_lowercase();
        if self.taken.insert(folded.clone()) {
            return (name, false);
        }
        let mut suffix = self.next.get(&folded).copied().unwrap_or(1);
        loop {
            let candidate = format!("{name}~{suffix}");
            suffix = suffix.saturating_add(1);
            if self.taken.insert(candidate.to_lowercase()) {
                self.next.insert(folded, suffix);
                return (candidate, true);
            }
        }
    }
}

/// A name for a temporary upload that will be renamed into place.
///
/// An interrupted upload leaves this name behind rather than truncating the
/// file it replaces.
#[must_use]
pub fn temporary_name(final_name: &str, nonce: u64) -> String {
    let stem: String = final_name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .take(32)
        .collect();
    format!(".ca-upload-{stem}-{nonce:016x}.part")
}

/// A value that differs between one upload and the next.
#[must_use]
pub fn nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let step = COUNTER.fetch_add(1, Ordering::Relaxed);
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |span| u64::try_from(span.as_nanos()).unwrap_or(u64::MAX));
    clock ^ step.rotate_left(32)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_hostile_listing_name_never_becomes_a_path() {
        let root = VfsPath::root();
        for name in [
            "..",
            ".",
            "",
            "../escape",
            "/etc/passwd",
            "a/b",
            "a\\b",
            "C:/Windows",
            "bad\u{7}name",
        ] {
            assert!(
                child_path(&root, name).is_err(),
                "{name:?} became a usable path"
            );
        }
        assert_eq!(child_path(&root, "ok.txt").unwrap().as_str(), "ok.txt");
    }

    #[test]
    fn a_name_with_a_nul_is_refused() {
        assert!(child_path(&VfsPath::root(), "a\0b").is_err());
    }

    #[test]
    fn a_temporary_upload_name_is_a_single_component() {
        let name = temporary_name("../escape.txt", 1);
        assert!(crate::path::VfsPath::parse(&name).is_ok());
        assert!(!name.contains('/'));
    }

    #[test]
    fn a_service_with_no_connector_reports_why() {
        let context = RemoteContext::default();
        let cancel = crate::cancel::Cancel::new();
        #[allow(
            unused_mut,
            reason = "Subversion joins this list only in builds without its connector"
        )]
        let mut services = vec![
            ServiceProfile::Dropbox(profile::CloudProfile::default()),
            ServiceProfile::OneDrive(profile::CloudProfile::default()),
        ];
        #[cfg(not(feature = "subversion"))]
        services.push(ServiceProfile::Subversion(
            profile::SubversionProfile::default(),
        ));
        for service in services {
            let profile = RemoteProfile {
                service,
                ..RemoteProfile::default()
            };
            let Err(error) = connect(&profile, &context, &cancel) else {
                panic!("a service with no connector must not connect");
            };
            assert!(matches!(error, VfsError::Unsupported { .. }));
            assert!(error.to_string().len() > 40);
        }
    }
}
