//! Administrator policies.
//!
//! Policies come from a machine-wide store read once at startup and are never
//! written by the program, so the resolved struct exposes readers only. The
//! platform-specific sources land later; this module defines the keys, the
//! loader trait, an in-memory source, and the text file format used outside
//! Windows.

use std::collections::BTreeMap;

/// Path of the policy file on platforms without a registry.
pub const POLICY_FILE_PATH: &str = "/etc/compare-all.conf";

/// A documented policy key.
///
/// A policy key never reaches a stored document, so it carries no serde
/// derives and needs no unknown arm: a key this build does not understand is
/// dropped by [`PolicyKey::from_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PolicyKey {
    /// Suppress every update check, the menu command that starts one, the
    /// related option, and the notification a new version would raise.
    DisableCheckForUpdates,
    /// Disable every remote storage profile. A read-only revision-control
    /// profile is exempt.
    DisableRemoteProfiles,
    /// Keep passwords in memory only and hide every control offering to save
    /// one.
    DisableSavedPasswords,
}

impl PolicyKey {
    /// Every key this build understands.
    pub const ALL: &'static [PolicyKey] = &[
        PolicyKey::DisableCheckForUpdates,
        PolicyKey::DisableRemoteProfiles,
        PolicyKey::DisableSavedPasswords,
    ];

    /// The name the policy carries in the machine-wide store.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            PolicyKey::DisableCheckForUpdates => "DisableCheckForUpdates",
            PolicyKey::DisableRemoteProfiles => "DisableRemoteProfiles",
            PolicyKey::DisableSavedPasswords => "DisableSavedPasswords",
        }
    }

    /// Reads a key from its stored name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        PolicyKey::ALL
            .iter()
            .copied()
            .find(|key| key.name() == name)
    }
}

/// Resolved policies. Every field is off unless a policy store enables it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdminPolicies {
    check_for_updates_disabled: bool,
    remote_profiles_disabled: bool,
    saved_passwords_disabled: bool,
}

impl AdminPolicies {
    /// Builds resolved policies from a set of enabled keys.
    #[must_use]
    pub fn from_keys(enabled: &BTreeMap<PolicyKey, bool>) -> Self {
        let get = |key: PolicyKey| enabled.get(&key).copied().unwrap_or(false);
        Self {
            check_for_updates_disabled: get(PolicyKey::DisableCheckForUpdates),
            remote_profiles_disabled: get(PolicyKey::DisableRemoteProfiles),
            saved_passwords_disabled: get(PolicyKey::DisableSavedPasswords),
        }
    }

    /// Whether the named policy is in force.
    #[must_use]
    pub fn is_set(&self, key: PolicyKey) -> bool {
        match key {
            PolicyKey::DisableCheckForUpdates => self.check_for_updates_disabled,
            PolicyKey::DisableRemoteProfiles => self.remote_profiles_disabled,
            PolicyKey::DisableSavedPasswords => self.saved_passwords_disabled,
        }
    }

    /// Update checks are suppressed.
    #[must_use]
    pub fn check_for_updates_disabled(&self) -> bool {
        self.check_for_updates_disabled
    }

    /// Remote storage profiles are unavailable.
    #[must_use]
    pub fn remote_profiles_disabled(&self) -> bool {
        self.remote_profiles_disabled
    }

    /// Passwords may not be written to disk.
    #[must_use]
    pub fn saved_passwords_disabled(&self) -> bool {
        self.saved_passwords_disabled
    }
}

/// Failure to read the policy store.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The policy store could not be read.
    #[error("cannot read policy store: {0}")]
    Unreadable(String),
}

/// A source of administrator policies.
///
/// A machine-wide source is consulted first and a per-user source refines it,
/// so an implementation covering both returns the combined result.
pub trait PolicyLoader {
    /// Reads the policy store.
    ///
    /// # Errors
    /// Returns [`PolicyError`] when the store exists but cannot be read.
    fn load(&self) -> Result<AdminPolicies, PolicyError>;
}

/// A policy source held in memory, for tests and for builds without a policy
/// store.
#[derive(Debug, Clone, Default)]
pub struct InMemoryPolicySource {
    values: BTreeMap<PolicyKey, bool>,
}

impl InMemoryPolicySource {
    /// A source with no policy in force.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets one policy and returns the source, for chaining.
    #[must_use]
    pub fn with(mut self, key: PolicyKey, enabled: bool) -> Self {
        self.values.insert(key, enabled);
        self
    }

    /// Reads policies from the text form used outside Windows: one
    /// `Name=yes` or `Name=no` per line, with `#` starting a comment.
    ///
    /// Unknown names and malformed lines are ignored so a file written for a
    /// newer build still applies the keys this one understands.
    #[must_use]
    pub fn from_conf(text: &str) -> Self {
        let mut values = BTreeMap::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let Some(key) = PolicyKey::from_name(name.trim()) else {
                continue;
            };
            let value = value.trim();
            if value.eq_ignore_ascii_case("yes") || value == "1" {
                values.insert(key, true);
            } else if value.eq_ignore_ascii_case("no") || value == "0" {
                values.insert(key, false);
            }
        }
        Self { values }
    }
}

impl PolicyLoader for InMemoryPolicySource {
    fn load(&self) -> Result<AdminPolicies, PolicyError> {
        Ok(AdminPolicies::from_keys(&self.values))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_in_force_by_default() {
        let policies = InMemoryPolicySource::new().load().unwrap();
        for key in PolicyKey::ALL {
            assert!(!policies.is_set(*key));
        }
    }

    #[test]
    fn a_set_policy_reads_back_through_both_accessors() {
        let policies = InMemoryPolicySource::new()
            .with(PolicyKey::DisableRemoteProfiles, true)
            .load()
            .unwrap();
        assert!(policies.remote_profiles_disabled());
        assert!(policies.is_set(PolicyKey::DisableRemoteProfiles));
        assert!(!policies.saved_passwords_disabled());
    }

    #[test]
    fn key_names_round_trip() {
        for key in PolicyKey::ALL {
            assert_eq!(PolicyKey::from_name(key.name()), Some(*key));
        }
        assert_eq!(PolicyKey::from_name("DisableEverything"), None);
    }

    #[test]
    fn the_text_form_reads_yes_no_and_ignores_comments() {
        let policies = InMemoryPolicySource::from_conf(
            "# a comment\n\
             DisableCheckForUpdates=yes\n\
             DisableRemoteProfiles = no\n\
             DisableSavedPasswords=1  # trailing comment\n\
             NotAPolicy=yes\n\
             malformed line\n",
        )
        .load()
        .unwrap();

        assert!(policies.check_for_updates_disabled());
        assert!(!policies.remote_profiles_disabled());
        assert!(policies.saved_passwords_disabled());
    }

    #[test]
    fn a_custom_loader_satisfies_the_trait() {
        struct AlwaysLocked;
        impl PolicyLoader for AlwaysLocked {
            fn load(&self) -> Result<AdminPolicies, PolicyError> {
                let mut values = BTreeMap::new();
                for key in PolicyKey::ALL {
                    values.insert(*key, true);
                }
                Ok(AdminPolicies::from_keys(&values))
            }
        }

        let policies = AlwaysLocked.load().unwrap();
        assert!(PolicyKey::ALL.iter().all(|key| policies.is_set(*key)));
    }
}
