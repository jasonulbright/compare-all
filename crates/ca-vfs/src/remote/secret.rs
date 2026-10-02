//! Secret values and the store a profile reads them from.
//!
//! A profile never carries a password, a key passphrase or an access key. It
//! carries a [`SecretRef`], and a [`SecretStore`] turns that reference into a
//! [`Secret`] at connect time. The separation keeps secrets out of anything
//! that is serialized, logged or printed: `SecretRef` is the only part with a
//! `Serialize` implementation, and `Secret` prints as `***` in both `Debug`
//! and `Display`.

use std::collections::BTreeMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// A secret value held in memory for the life of a connection.
///
/// `Debug` and `Display` print a fixed mask, so a secret that reaches a log
/// line, an error message or a panic payload carries no value. The plain text
/// is reachable only through [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

/// What every redacted rendering of a secret prints.
pub const REDACTED: &str = "***";

impl Secret {
    /// Wrap a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The plain text.
    ///
    /// Callers pass the result straight to the protocol that needs it and do
    /// not store, format or copy it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The plain text as bytes.
    #[must_use]
    pub fn expose_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// True when the value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// A name under which a store holds one secret.
///
/// The reference is serialized with the profile; the value behind it is not.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretRef(String);

impl SecretRef {
    /// A reference to the secret stored under `id`.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier the store is keyed by.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.0
    }

    /// True when no secret is named.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Supplies the value behind a [`SecretRef`].
///
/// Two implementations ship: the in-memory store used by tests and by a
/// session that holds a password only for as long as it is open, and, under
/// the `os-keychain` feature, the operating system credential store.
pub trait SecretStore: Send + Sync {
    /// The secret stored under `reference`, or `None` when nothing is stored.
    fn secret(&self, reference: &SecretRef) -> Option<Secret>;
}

/// A store that keeps secrets in memory only.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    values: RwLock<BTreeMap<String, String>>,
}

impl MemorySecretStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `value` under `id`, replacing anything already there.
    pub fn insert(&self, id: impl Into<String>, value: impl Into<String>) {
        if let Ok(mut map) = self.values.write() {
            map.insert(id.into(), value.into());
        }
    }

    /// Forget the secret stored under `id`.
    pub fn remove(&self, id: &str) {
        if let Ok(mut map) = self.values.write() {
            map.remove(id);
        }
    }
}

impl SecretStore for MemorySecretStore {
    fn secret(&self, reference: &SecretRef) -> Option<Secret> {
        let map = self.values.read().ok()?;
        map.get(reference.id()).map(Secret::new)
    }
}

/// A store backed by the operating system credential store.
///
/// The reference is split at the last separator: the part before it is the
/// account name and the part after it is the entry name, so `shelf/password`
/// reads the entry `password` of the account `shelf`. A reference with no
/// separator names an entry of the default account.
///
/// Nothing is written and nothing is deleted. The credential store is filled
/// by the platform's own tool, so a failed or refused read is a missing
/// secret, which the caller reports by name.
#[cfg(feature = "os-keychain")]
#[derive(Debug, Clone)]
pub struct OsSecretStore {
    service: String,
}

#[cfg(feature = "os-keychain")]
impl OsSecretStore {
    /// A store whose entries live under `service`.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Whether the platform credential store could be opened.
    ///
    /// A caller states the reason rather than reporting every secret as
    /// missing.
    ///
    /// # Errors
    /// Returns the text of the platform's own failure.
    pub fn availability() -> Result<(), String> {
        match keyring::Entry::store_status() {
            Ok(()) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// The service and account one reference addresses.
    fn address(&self, reference: &SecretRef) -> (String, String) {
        match reference.id().rsplit_once(['/', '\\']) {
            Some((account, entry)) => (format!("{}:{account}", self.service), entry.to_owned()),
            None => (self.service.clone(), reference.id().to_owned()),
        }
    }
}

#[cfg(feature = "os-keychain")]
impl SecretStore for OsSecretStore {
    fn secret(&self, reference: &SecretRef) -> Option<Secret> {
        if reference.is_empty() {
            return None;
        }
        let (service, account) = self.address(reference);
        let entry = keyring::Entry::new(&service, &account).ok()?;
        entry.get_password().ok().map(Secret::new)
    }
}

/// A store that holds nothing, for a connection that needs no secret.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSecrets;

impl SecretStore for NoSecrets {
    fn secret(&self, _reference: &SecretRef) -> Option<Secret> {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_secret_never_prints_its_value() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), REDACTED);
        assert_eq!(format!("{secret}"), REDACTED);
        assert!(!format!("{secret:?} {secret}").contains("hunter2"));
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn the_store_resolves_a_reference() {
        let store = MemorySecretStore::new();
        store.insert("profile/a", "hunter2");
        let found = store.secret(&SecretRef::new("profile/a")).unwrap();
        assert_eq!(found.expose(), "hunter2");
        assert!(store.secret(&SecretRef::new("profile/b")).is_none());
    }
}
