//! Administrator policies.
//!
//! Policies come from a machine-wide store read once at startup and are never
//! written by the program, so the resolved struct exposes readers only. This
//! module defines the keys, the loader trait, an in-memory source, the text
//! file source used outside Windows, and the registry source used on Windows.

use std::collections::BTreeMap;
use std::path::PathBuf;

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

    /// Every restricting policy in force, as when the policy store exists but
    /// cannot be read.
    #[must_use]
    pub fn all_restricted() -> Self {
        Self {
            check_for_updates_disabled: true,
            remote_profiles_disabled: true,
            saved_passwords_disabled: true,
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

/// The policy file read on platforms without a registry.
///
/// A missing file means no policy is in force. Bytes that are not UTF-8 and
/// lines that do not parse are ignored.
#[derive(Debug, Clone)]
pub struct FilePolicySource {
    path: PathBuf,
}

impl FilePolicySource {
    /// A source reading `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// A source reading [`POLICY_FILE_PATH`].
    #[must_use]
    pub fn system() -> Self {
        Self::new(POLICY_FILE_PATH)
    }
}

impl PolicyLoader for FilePolicySource {
    fn load(&self) -> Result<AdminPolicies, PolicyError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => InMemoryPolicySource::from_conf(&String::from_utf8_lossy(&bytes)).load(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(AdminPolicies::default())
            }
            Err(error) => Err(PolicyError::Unreadable(error.to_string())),
        }
    }
}

/// Registry key holding the policy values, below `HKEY_LOCAL_MACHINE` and
/// `HKEY_CURRENT_USER`.
#[cfg(windows)]
pub const POLICY_REGISTRY_PATH: &str = "SOFTWARE\\Policies\\compare-all";

/// The policy values in the registry.
///
/// Each key is a `REG_DWORD` named as the key; a non-zero value enables it.
/// The locations are read in order and a key enabled in any of them is in
/// force, so a per-user value of zero never clears a machine value of one. A
/// missing key, a missing value, and a value of another type count as not set.
/// A key that exists but cannot be opened, or a value that cannot be read for
/// reason other than absence or a different type, is an error.
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct RegistryPolicySource {
    locations: Vec<(winreg::HKEY, String)>,
}

#[cfg(windows)]
impl RegistryPolicySource {
    /// The machine location, then the user location.
    #[must_use]
    pub fn system() -> Self {
        Self::with_locations(vec![
            (
                winreg::enums::HKEY_LOCAL_MACHINE,
                POLICY_REGISTRY_PATH.to_owned(),
            ),
            (
                winreg::enums::HKEY_CURRENT_USER,
                POLICY_REGISTRY_PATH.to_owned(),
            ),
        ])
    }

    /// Reads the given predefined roots and subkey paths, in order.
    #[must_use]
    pub fn with_locations(locations: Vec<(winreg::HKEY, String)>) -> Self {
        Self { locations }
    }
}

/// `ERROR_BAD_FILE_TYPE`, which the registry reader reports for a value whose
/// type is not `REG_DWORD`.
#[cfg(windows)]
const WRONG_VALUE_TYPE: i32 = 222;

/// Whether a failed value read stops the load. An absent value and a value of
/// another type than `REG_DWORD` count as not set; any other failure means the
/// policy cannot be known.
#[cfg(windows)]
fn value_error_is_fatal(error: &std::io::Error) -> bool {
    error.kind() != std::io::ErrorKind::NotFound && error.raw_os_error() != Some(WRONG_VALUE_TYPE)
}

#[cfg(windows)]
impl PolicyLoader for RegistryPolicySource {
    fn load(&self) -> Result<AdminPolicies, PolicyError> {
        use winreg::enums::{KEY_READ, KEY_WOW64_64KEY};
        use winreg::RegKey;

        let mut enabled = BTreeMap::new();
        for (root, path) in &self.locations {
            let key = match RegKey::predef(*root)
                .open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)
            {
                Ok(key) => key,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(PolicyError::Unreadable(format!("{path}: {error}")));
                }
            };
            for policy in PolicyKey::ALL {
                match key.get_value::<u32, _>(policy.name()) {
                    Ok(value) if value != 0 => {
                        enabled.insert(*policy, true);
                    }
                    Err(error) if value_error_is_fatal(&error) => {
                        return Err(PolicyError::Unreadable(format!(
                            "{path}: {}: {error}",
                            policy.name()
                        )));
                    }
                    Ok(_) | Err(_) => {}
                }
            }
        }
        Ok(AdminPolicies::from_keys(&enabled))
    }
}

/// Policies read at startup, with the reason when the store was unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyLoad {
    /// The policies in force.
    pub policies: AdminPolicies,
    /// Why every restricting policy was put in force, when the store exists
    /// but could not be read.
    pub warning: Option<String>,
}

/// Reads `loader` once. A store that exists but cannot be read puts every
/// restricting policy in force and reports the reason.
#[must_use]
pub fn load_policies(loader: &dyn PolicyLoader) -> PolicyLoad {
    match loader.load() {
        Ok(policies) => PolicyLoad {
            policies,
            warning: None,
        },
        Err(error) => PolicyLoad {
            policies: AdminPolicies::all_restricted(),
            warning: Some(format!(
                "{error}. Every administrator policy is in force until the store can be read."
            )),
        },
    }
}

/// Reads the policies of this machine once, with the reason when the store
/// was unreadable.
#[must_use]
pub fn load_system_policies_reported() -> PolicyLoad {
    #[cfg(windows)]
    let loaded = load_policies(&RegistryPolicySource::system());
    #[cfg(not(windows))]
    let loaded = load_policies(&FilePolicySource::system());
    loaded
}

/// Reads the policies of this machine once.
///
/// A store that exists but cannot be read puts every policy in force.
#[must_use]
pub fn load_system_policies() -> AdminPolicies {
    load_system_policies_reported().policies
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
    fn a_missing_file_sets_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let policies = FilePolicySource::new(dir.path().join("absent.conf"))
            .load()
            .unwrap();
        assert_eq!(policies, AdminPolicies::default());
    }

    #[test]
    fn a_file_is_read_and_bad_lines_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compare-all.conf");
        std::fs::write(
            &path,
            b"DisableRemoteProfiles=yes\n\xff\xfe garbage\nDisableSavedPasswords=maybe\n=\n",
        )
        .unwrap();
        let policies = FilePolicySource::new(&path).load().unwrap();
        assert!(policies.remote_profiles_disabled());
        assert!(!policies.saved_passwords_disabled());
        assert!(!policies.check_for_updates_disabled());
    }

    #[test]
    fn an_unreadable_store_puts_every_policy_in_force_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let load = load_policies(&FilePolicySource::new(dir.path()));
        for key in PolicyKey::ALL {
            assert!(load.policies.is_set(*key));
        }
        assert!(load.warning.unwrap().contains("cannot read policy store"));
    }

    #[test]
    fn a_missing_store_puts_nothing_in_force_and_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let load = load_policies(&FilePolicySource::new(dir.path().join("absent.conf")));
        assert_eq!(load.policies, AdminPolicies::default());
        assert!(load.warning.is_none());
    }

    #[test]
    fn an_unreadable_file_is_an_error_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let result = FilePolicySource::new(dir.path()).load();
        assert!(matches!(result, Err(PolicyError::Unreadable(_))));
    }

    #[cfg(windows)]
    mod registry {
        use super::*;
        use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS};
        use winreg::RegKey;

        struct TestRoot(String);

        impl TestRoot {
            fn new(name: &str) -> Self {
                let path = format!(
                    "SOFTWARE\\compare-all-test-policies\\{}-{name}",
                    std::process::id()
                );
                let root = Self(path);
                root.remove();
                RegKey::predef(HKEY_CURRENT_USER)
                    .create_subkey(&root.0)
                    .unwrap();
                root
            }

            fn key(&self) -> RegKey {
                RegKey::predef(HKEY_CURRENT_USER)
                    .open_subkey_with_flags(&self.0, KEY_ALL_ACCESS)
                    .unwrap()
            }

            fn remove(&self) {
                let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
            }

            fn location(&self) -> (winreg::HKEY, String) {
                (HKEY_CURRENT_USER, self.0.clone())
            }
        }

        impl Drop for TestRoot {
            fn drop(&mut self) {
                self.remove();
            }
        }

        #[test]
        fn only_an_absent_value_or_another_type_counts_as_not_set() {
            use std::io::{Error, ErrorKind};
            assert!(!value_error_is_fatal(&Error::from(ErrorKind::NotFound)));
            assert!(!value_error_is_fatal(&Error::from_raw_os_error(2)));
            assert!(!value_error_is_fatal(&Error::from_raw_os_error(
                WRONG_VALUE_TYPE
            )));
            assert!(value_error_is_fatal(&Error::from(
                ErrorKind::PermissionDenied
            )));
            assert!(value_error_is_fatal(&Error::from(ErrorKind::InvalidData)));
            assert!(value_error_is_fatal(&Error::from_raw_os_error(1450)));
            assert!(value_error_is_fatal(&Error::other("read failed")));
        }

        #[test]
        fn a_missing_key_sets_nothing() {
            let source = RegistryPolicySource::with_locations(vec![(
                HKEY_CURRENT_USER,
                "SOFTWARE\\compare-all-test-policies\\absent".to_owned(),
            )]);
            assert_eq!(source.load().unwrap(), AdminPolicies::default());
        }

        #[test]
        fn non_zero_dwords_enable_and_zero_does_not() {
            let root = TestRoot::new("dword");
            root.key()
                .set_value("DisableCheckForUpdates", &1u32)
                .unwrap();
            root.key()
                .set_value("DisableRemoteProfiles", &7u32)
                .unwrap();
            root.key()
                .set_value("DisableSavedPasswords", &0u32)
                .unwrap();
            let policies = RegistryPolicySource::with_locations(vec![root.location()])
                .load()
                .unwrap();
            assert!(policies.check_for_updates_disabled());
            assert!(policies.remote_profiles_disabled());
            assert!(!policies.saved_passwords_disabled());
        }

        #[test]
        fn values_of_other_types_are_ignored() {
            let root = TestRoot::new("types");
            root.key()
                .set_value("DisableCheckForUpdates", &"1")
                .unwrap();
            root.key()
                .set_value("DisableRemoteProfiles", &1u64)
                .unwrap();
            root.key()
                .set_raw_value(
                    "DisableSavedPasswords",
                    &winreg::RegValue {
                        vtype: winreg::enums::REG_BINARY,
                        bytes: vec![1, 0, 0, 0].into(),
                    },
                )
                .unwrap();
            let policies = RegistryPolicySource::with_locations(vec![root.location()])
                .load()
                .unwrap();
            assert_eq!(policies, AdminPolicies::default());
        }

        #[test]
        fn the_user_location_cannot_clear_the_machine_location() {
            let machine = TestRoot::new("machine");
            let user = TestRoot::new("user");
            machine
                .key()
                .set_value("DisableCheckForUpdates", &1u32)
                .unwrap();
            user.key()
                .set_value("DisableCheckForUpdates", &0u32)
                .unwrap();
            user.key()
                .set_value("DisableSavedPasswords", &1u32)
                .unwrap();
            let policies =
                RegistryPolicySource::with_locations(vec![machine.location(), user.location()])
                    .load()
                    .unwrap();
            assert!(policies.check_for_updates_disabled());
            assert!(policies.saved_passwords_disabled());
            assert!(!policies.remote_profiles_disabled());
        }
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
