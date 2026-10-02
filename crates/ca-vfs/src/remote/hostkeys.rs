//! The store of secure shell host keys this program accepts.
//!
//! The store is a file the caller names; this crate never chooses a location.
//! Each line holds a host, a key type and the key, in the same shape the
//! common secure shell clients use, so an operator can read it.
//!
//! Three outcomes matter to a connection:
//!
//! - the offered key matches a recorded one, and the connection proceeds;
//! - nothing is recorded for the host, and the connection fails with
//!   [`VfsError::UnknownHostKey`] carrying the fingerprint, so the interface
//!   can show it and record the answer;
//! - a key is recorded and differs, and the connection fails with
//!   [`VfsError::HostKeyChanged`]. That is never retried automatically.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::error::{VfsError, VfsResult};

/// Accepted host keys, kept in one file.
#[derive(Debug, Clone)]
pub struct KnownHosts {
    path: PathBuf,
}

/// One recorded key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostEntry {
    /// The host field as the file holds it: a plain label, or the hashed form
    /// `|1|<salt>|<digest>` that the common client writes.
    pub host: String,
    /// Key algorithm name, as the protocol spells it.
    pub key_type: String,
    /// The key itself, base64 encoded.
    pub key: String,
}

/// Prefix of the only hashed host form the file format defines.
const HASH_PREFIX: &str = "|1|";

impl KnownHostEntry {
    /// True when this entry stands for `label`.
    ///
    /// A hashed entry stores the salt and the HMAC-SHA1 of the label under
    /// that salt, so the label is recovered from neither and must be hashed
    /// again to be compared.
    #[must_use]
    pub fn matches_host(&self, label: &str) -> bool {
        match hashed_parts(&self.host) {
            Some((salt, digest)) => hash_host(&salt, label) == digest,
            None => self.host == label,
        }
    }

    /// True when the host field is stored hashed rather than in the clear.
    #[must_use]
    pub fn is_hashed(&self) -> bool {
        hashed_parts(&self.host).is_some()
    }
}

/// The salt and the digest of a hashed host field.
fn hashed_parts(host: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let rest = host.strip_prefix(HASH_PREFIX)?;
    let (salt, digest) = rest.split_once('|')?;
    let engine = base64::engine::general_purpose::STANDARD;
    let salt = engine.decode(salt).ok()?;
    let digest = engine.decode(digest).ok()?;
    // An empty salt or digest matches nothing, and a digest of another length
    // was written by something other than this format.
    if salt.is_empty() || digest.len() != 20 {
        return None;
    }
    Some((salt, digest))
}

/// HMAC-SHA1 of the host label under the stored salt.
fn hash_host(salt: &[u8], label: &str) -> Vec<u8> {
    use hmac::{KeyInit as _, Mac as _};
    // The key length is whatever the file recorded, which this construction
    // accepts for any length.
    let Ok(mut mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(salt) else {
        return Vec::new();
    };
    mac.update(label.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// The fingerprint form the secure shell tools print: `SHA256:` and the
/// base64 of the digest, with no padding.
#[must_use]
pub fn ssh_fingerprint(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    format!("SHA256:{encoded}")
}

/// How a host is written in the store: the name alone on the standard port,
/// and `[name]:port` otherwise.
#[must_use]
pub fn host_label(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

impl KnownHosts {
    /// A store backed by `path`. The file need not exist yet.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the store is kept.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every recorded entry, in file order.
    ///
    /// A line that does not parse is skipped rather than failing the read, so
    /// one damaged line does not lock every host out.
    ///
    /// # Errors
    /// Returns [`VfsError::Io`] when the file exists and cannot be read.
    pub fn entries(&self) -> VfsResult<Vec<KnownHostEntry>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(VfsError::Io(error)),
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // A marker line changes what the entry means, so it is not read as
            // an ordinary entry. The host stays unrecorded, which fails the
            // check.
            if line.starts_with('@') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(host), Some(key_type), Some(key)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            // A pattern covers hosts this store cannot enumerate, so it is not
            // treated as naming any one of them. A hashed field carries none
            // of these characters, so it passes.
            if host.contains(['*', '?', ',', '!']) {
                continue;
            }
            // A hashed field that does not parse stands for no host at all.
            if host.starts_with(HASH_PREFIX) && hashed_parts(host).is_none() {
                continue;
            }
            out.push(KnownHostEntry {
                host: host.to_owned(),
                key_type: key_type.to_owned(),
                key: key.to_owned(),
            });
        }
        Ok(out)
    }

    /// Check an offered key against the store.
    ///
    /// # Errors
    /// Returns [`VfsError::UnknownHostKey`] when nothing is recorded for the
    /// host, and [`VfsError::HostKeyChanged`] when a recorded key differs.
    pub fn verify(&self, host: &str, port: u16, key_type: &str, key: &[u8]) -> VfsResult<()> {
        let label = host_label(host, port);
        let offered = base64::engine::general_purpose::STANDARD.encode(key);
        let entries = self.entries()?;
        let mut recorded: Option<String> = None;
        for entry in entries {
            if !entry.matches_host(&label) {
                continue;
            }
            if entry.key == offered {
                return Ok(());
            }
            if recorded.is_none() || entry.key_type == key_type {
                recorded = Some(entry.key);
            }
        }
        match recorded {
            Some(known) => Err(VfsError::HostKeyChanged {
                host: label,
                known: ssh_fingerprint(
                    &base64::engine::general_purpose::STANDARD
                        .decode(known)
                        .unwrap_or_default(),
                ),
                offered: ssh_fingerprint(key),
            }),
            None => Err(VfsError::UnknownHostKey {
                host: label,
                fingerprint: ssh_fingerprint(key),
            }),
        }
    }

    /// Record a key the caller accepted.
    ///
    /// The whole file is written to a temporary name beside it and renamed
    /// over it, so a reader never sees a part-written file and a writer that
    /// stops part way leaves the previous content in place. A file whose last
    /// line has no line ending is repaired rather than appended onto, which
    /// would otherwise join two entries into one unusable line.
    ///
    /// # Errors
    /// Returns [`VfsError::Io`] when the file cannot be written.
    pub fn record(&self, host: &str, port: u16, key_type: &str, key: &[u8]) -> VfsResult<()> {
        let label = host_label(host, port);
        if label.split_whitespace().count() != 1
            || key_type.split_whitespace().count() != 1
            || key_type.is_empty()
            // A label written in the clear that starts with the hashed prefix
            // would be read back as a hashed field, which stands for a
            // different host.
            || label.starts_with(HASH_PREFIX)
        {
            return Err(VfsError::protocol(
                "a host key entry names a host or a key type that cannot be recorded".to_owned(),
            ));
        }
        match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                fs::create_dir_all(parent)?;
            }
            _ => {}
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(key);
        let mut text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(VfsError::Io(error)),
        };
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        writeln!(text, "{label} {key_type} {encoded}")
            .map_err(|_| VfsError::protocol("the entry could not be written".to_owned()))?;

        ca_io::write_atomic_private(&self.path, text.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn an_unrecorded_host_reports_its_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHosts::new(dir.path().join("known_hosts"));
        let error = store
            .verify("example", 22, "ssh-ed25519", b"key")
            .unwrap_err();
        let VfsError::UnknownHostKey { host, fingerprint } = error else {
            panic!("expected an unknown host key");
        };
        assert_eq!(host, "example");
        assert_eq!(fingerprint, ssh_fingerprint(b"key"));
    }

    #[test]
    fn a_recorded_key_verifies_and_a_changed_one_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHosts::new(dir.path().join("known_hosts"));
        store
            .record("example", 2222, "ssh-ed25519", b"key")
            .unwrap();
        store
            .verify("example", 2222, "ssh-ed25519", b"key")
            .unwrap();
        let error = store
            .verify("example", 2222, "ssh-ed25519", b"other")
            .unwrap_err();
        assert!(matches!(error, VfsError::HostKeyChanged { .. }));
        assert!(store.verify("example", 22, "ssh-ed25519", b"key").is_err());
    }

    /// The host field a client writes for `label` under `salt`.
    fn hashed_field(salt: &[u8], label: &str) -> String {
        let engine = base64::engine::general_purpose::STANDARD;
        format!(
            "|1|{}|{}",
            engine.encode(salt),
            engine.encode(hash_host(salt, label))
        )
    }

    #[test]
    fn a_hashed_entry_verifies_the_host_it_stands_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = b"key-bytes";
        let encoded = base64::engine::general_purpose::STANDARD.encode(key);
        let field = hashed_field(b"salt-value", "example.test");
        fs::write(&path, format!("{field} ssh-ed25519 {encoded}\n")).unwrap();

        let store = KnownHosts::new(&path);
        assert!(store.entries().unwrap()[0].is_hashed());
        store
            .verify("example.test", 22, "ssh-ed25519", key)
            .unwrap();
    }

    #[test]
    fn a_hashed_entry_for_another_host_leaves_this_one_unrecorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"key-bytes");
        let field = hashed_field(b"salt-value", "other.test");
        fs::write(&path, format!("{field} ssh-ed25519 {encoded}\n")).unwrap();

        let store = KnownHosts::new(&path);
        let error = store
            .verify("example.test", 22, "ssh-ed25519", b"key-bytes")
            .unwrap_err();
        assert!(matches!(error, VfsError::UnknownHostKey { .. }), "{error}");
    }

    #[test]
    fn a_hashed_entry_with_a_different_key_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"recorded");
        let field = hashed_field(b"salt-value", "[example.test]:2222");
        fs::write(&path, format!("{field} ssh-ed25519 {encoded}\n")).unwrap();

        let store = KnownHosts::new(&path);
        let error = store
            .verify("example.test", 2222, "ssh-ed25519", b"offered")
            .unwrap_err();
        assert!(matches!(error, VfsError::HostKeyChanged { .. }), "{error}");
    }

    #[test]
    fn a_hashed_field_that_does_not_parse_stands_for_no_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        fs::write(
            &path,
            "|1|c2FsdA==|aGFzaA== ssh-ed25519 AAAAa\n\
             |1|no-separator ssh-ed25519 AAAAa\n\
             |1||aGFzaA== ssh-ed25519 AAAAa\n",
        )
        .unwrap();
        let store = KnownHosts::new(&path);
        assert!(store.entries().unwrap().is_empty());
    }

    #[test]
    fn a_label_that_looks_hashed_is_never_recorded_in_the_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHosts::new(dir.path().join("known_hosts"));
        assert!(store
            .record("|1|salt|digest", 22, "ssh-ed25519", b"key")
            .is_err());
    }
}
