//! Registry records: export files, the live registry, comparison and edit plans.
//!
//! Three sources feed one tree. A `.reg` export file is parsed by
//! [`reg_file::RegFile`]. The live registry of the running machine is read by
//! [`live::read`], which exists on Windows only; every other platform returns a
//! typed [`crate::error::RecordError::Unsupported`]. Both produce a
//! [`crate::record::RecordTree`], so the comparison does not know which source
//! it was given.

pub mod live;
pub mod plan;
pub mod reg_file;
pub mod value;

use crate::compare::{compare_trees, AlignOptions, TreeDiff};
use crate::error::{RecordError, Result};
use crate::limits::{Limits, Unknown};
use crate::record::RecordTree;
use serde::{Deserialize, Serialize};

pub use plan::{DeleteForm, EditOp, EditPlan, LiveStep, Side, StepCounts};
pub use reg_file::{RegEntry, RegFile, RegFileVersion, RegKeyBlock};
pub use value::{ValueData, ValueKind, ValueName};

/// Prefix that marks a live registry address.
const LIVE_PREFIX: &str = "reg:\\\\";

/// A root of the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Hive {
    /// `HKEY_CLASSES_ROOT`.
    ClassesRoot,
    /// `HKEY_CURRENT_USER`.
    CurrentUser,
    /// `HKEY_LOCAL_MACHINE`.
    LocalMachine,
    /// `HKEY_USERS`.
    Users,
    /// `HKEY_CURRENT_CONFIG`.
    CurrentConfig,
    /// `HKEY_PERFORMANCE_DATA`.
    PerformanceData,
}

impl Hive {
    /// The full name of the hive.
    #[must_use]
    pub const fn full_name(self) -> &'static str {
        match self {
            Self::ClassesRoot => "HKEY_CLASSES_ROOT",
            Self::CurrentUser => "HKEY_CURRENT_USER",
            Self::LocalMachine => "HKEY_LOCAL_MACHINE",
            Self::Users => "HKEY_USERS",
            Self::CurrentConfig => "HKEY_CURRENT_CONFIG",
            Self::PerformanceData => "HKEY_PERFORMANCE_DATA",
        }
    }

    /// The short name of the hive.
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::ClassesRoot => "HKCR",
            Self::CurrentUser => "HKCU",
            Self::LocalMachine => "HKLM",
            Self::Users => "HKU",
            Self::CurrentConfig => "HKCC",
            Self::PerformanceData => "HKPD",
        }
    }

    /// Match a full or short hive name, ignoring case.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        const ALL: [Hive; 6] = [
            Hive::ClassesRoot,
            Hive::CurrentUser,
            Hive::LocalMachine,
            Hive::Users,
            Hive::CurrentConfig,
            Hive::PerformanceData,
        ];
        ALL.into_iter().find(|hive| {
            hive.full_name().eq_ignore_ascii_case(name)
                || hive.short_name().eq_ignore_ascii_case(name)
        })
    }
}

/// Which of the two registry views a 64 bit system exposes to read.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryView {
    /// The view that matches the running process.
    #[default]
    Native,
    /// The 32 bit view.
    Bit32,
    /// The 64 bit view.
    Bit64,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// An address that names a live registry key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySpec {
    /// Machine the key lives on. Absent means the running machine.
    pub machine: Option<String>,
    /// Root of the key.
    pub hive: Hive,
    /// Path under the hive. Empty addresses the hive itself.
    pub sub_key: String,
}

impl RegistrySpec {
    /// Parse a live registry address.
    ///
    /// Two forms are accepted. `reg:\\HIVE\Key` names a key on the running
    /// machine. `reg:\\Machine\HIVE\Key` names a key on another machine. The
    /// prefix may be left off.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::InvalidSpec`] when no hive name is present.
    pub fn parse(spec: &str) -> Result<Self> {
        let trimmed = spec.trim();
        let body = strip_prefix_ignore_case(trimmed, LIVE_PREFIX).unwrap_or(trimmed);
        let body = body.trim_start_matches('\\');
        let mut parts = body.splitn(2, '\\');
        let Some(first) = parts.next() else {
            return Err(RecordError::InvalidSpec(spec.to_owned()));
        };
        let rest = parts.next().unwrap_or_default();
        if let Some(hive) = Hive::parse(first) {
            return Ok(Self {
                machine: None,
                hive,
                sub_key: rest.trim_matches('\\').to_owned(),
            });
        }
        let mut tail = rest.splitn(2, '\\');
        let Some(second) = tail.next() else {
            return Err(RecordError::InvalidSpec(spec.to_owned()));
        };
        let Some(hive) = Hive::parse(second) else {
            return Err(RecordError::InvalidSpec(spec.to_owned()));
        };
        if first.is_empty() {
            return Err(RecordError::InvalidSpec(spec.to_owned()));
        }
        Ok(Self {
            machine: Some(first.to_owned()),
            hive,
            sub_key: tail
                .next()
                .unwrap_or_default()
                .trim_matches('\\')
                .to_owned(),
        })
    }

    /// True when the address names a machine other than the running one.
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        self.machine.is_some()
    }

    /// The full key path, hive first.
    #[must_use]
    pub fn key_path(&self) -> String {
        if self.sub_key.is_empty() {
            return self.hive.full_name().to_owned();
        }
        format!("{}\\{}", self.hive.full_name(), self.sub_key)
    }

    /// The address written back in the `reg:` form.
    #[must_use]
    pub fn to_spec_string(&self) -> String {
        match &self.machine {
            Some(machine) => format!("{LIVE_PREFIX}{machine}\\{}", self.key_path()),
            None => format!("{LIVE_PREFIX}{}", self.key_path()),
        }
    }
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() < prefix.len() {
        return None;
    }
    let head = value.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        value.get(prefix.len()..)
    } else {
        None
    }
}

/// How a live registry key is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LiveOptions {
    /// Which of the two views of a 64 bit system to read.
    pub view: RegistryView,
    /// Read the key and everything under it. Off reads one key only.
    pub recursive: bool,
    /// Allocation ceilings applied while reading.
    pub limits: Limits,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            view: RegistryView::Native,
            recursive: true,
            limits: Limits::default(),
            unknown: Unknown::new(),
        }
    }
}

/// Options of a registry comparison.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RegistryCompareOptions {
    /// Rules shared by every record comparison.
    pub align: AlignOptions,
    /// Report a key that only one side deletes as a difference.
    pub compare_deletions: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// Compare two registry trees.
#[must_use]
pub fn compare(
    left: &RecordTree,
    right: &RecordTree,
    options: &RegistryCompareOptions,
) -> TreeDiff {
    compare_trees(left, right, &options.align)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{Hive, RegistrySpec};

    #[test]
    fn a_local_address_names_a_hive_and_a_key() {
        let spec = RegistrySpec::parse(r"reg:\\HKEY_LOCAL_MACHINE\MyKey").unwrap();
        assert_eq!(spec.machine, None);
        assert_eq!(spec.hive, Hive::LocalMachine);
        assert_eq!(spec.sub_key, "MyKey");
    }

    #[test]
    fn a_remote_address_names_a_machine_first() {
        let spec = RegistrySpec::parse(r"reg:\\MyComputer\HKEY_USERS\MyKey").unwrap();
        assert_eq!(spec.machine.as_deref(), Some("MyComputer"));
        assert_eq!(spec.hive, Hive::Users);
        assert_eq!(spec.sub_key, "MyKey");
        assert!(spec.is_remote());
    }

    #[test]
    fn an_address_round_trips_through_its_string_form() {
        let spec = RegistrySpec::parse(r"HKCU\Software\Example").unwrap();
        assert_eq!(spec.key_path(), r"HKEY_CURRENT_USER\Software\Example");
        let again = RegistrySpec::parse(&spec.to_spec_string()).unwrap();
        assert_eq!(again, spec);
    }

    #[test]
    fn an_address_without_a_hive_is_refused() {
        assert!(RegistrySpec::parse(r"reg:\\NotAHive\Whatever").is_err());
    }
}
