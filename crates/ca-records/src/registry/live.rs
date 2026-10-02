//! Reading and writing the live registry.
//!
//! The readers open keys with query and enumerate rights only. The writer is a
//! separate path, [`apply_confirmed`], and it refuses before the first write
//! unless the caller states a consent, every step lies inside the base key of
//! the comparison, and a hive the system depends on has a second consent.
//!
//! Both exist on Windows only. Every other platform answers
//! [`RecordError::Unsupported`] with the reason, so a caller can report the
//! limit instead of guessing why nothing appeared.

use crate::error::{RecordError, Result};
use crate::record::RecordTree;
use crate::registry::plan::{is_within, LiveStep};
use crate::registry::reg_file::{RegEntry, RegFile, RegFileVersion, RegKeyBlock};
use crate::registry::value::ValueName;
use crate::registry::{Hive, LiveOptions, RegistrySpec, RegistryView};
use std::collections::BTreeMap;

/// Why a remote key cannot be opened.
const REMOTE_REASON: &str =
    "the registry wrapper offers no remote connection, so a key on another machine cannot be opened";

/// Read a live registry key into a record tree.
///
/// # Errors
///
/// Returns [`RecordError::Unsupported`] on a platform without a live registry
/// and for a remote address, [`RecordError::AccessDenied`] when the operating
/// system refuses the key, [`RecordError::NotFound`] when the key is absent,
/// and [`RecordError::LimitExceeded`] when the key holds more than the limits
/// allow.
pub fn read(spec: &RegistrySpec, options: &LiveOptions) -> Result<RecordTree> {
    check_readable(spec, options)?;
    platform::read(spec, options)
}

/// Read a live registry key as an export file model: one block for the key
/// and, when the options ask for it, one block for every key under it.
///
/// Blocks come parent first, and the keys and values of each level in name
/// order, so two exports of the same keys are equal.
///
/// # Errors
///
/// Same as [`read`].
pub fn export(spec: &RegistrySpec, options: &LiveOptions) -> Result<RegFile> {
    check_readable(spec, options)?;
    platform::export(spec, options)
}

fn check_readable(spec: &RegistrySpec, options: &LiveOptions) -> Result<()> {
    if spec.is_remote() {
        return Err(RecordError::unsupported(REMOTE_REASON));
    }
    if let RegistryView::Unknown(value) = &options.view {
        return Err(RecordError::unsupported(format!(
            "unknown registry view: {value}"
        )));
    }
    Ok(())
}

/// True when this build can read a live registry at all.
#[must_use]
pub const fn is_available() -> bool {
    cfg!(windows)
}

/// True when this build can read a registry on another machine.
///
/// The wrapper this crate uses offers no remote connection, so the answer is
/// always false and a remote address is refused with a reason.
#[must_use]
pub const fn remote_is_available() -> bool {
    false
}

/// The reason a remote address is refused.
#[must_use]
pub const fn remote_reason() -> &'static str {
    REMOTE_REASON
}

/// What the user agreed to before steps reach the live registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteConsent {
    /// The user confirmed the summary of the writes.
    pub confirmed: bool,
    /// The user confirmed a second time for a hive the system depends on.
    pub protected_hive_confirmed: bool,
}

/// True when writes to `hive` need a second consent.
///
/// `HKEY_CURRENT_CONFIG` is a view into `HKEY_LOCAL_MACHINE`, and
/// `HKEY_USERS` holds the settings of every account, so both count with the
/// two hives the system reads at start.
#[must_use]
pub const fn is_protected_hive(hive: Hive) -> bool {
    matches!(
        hive,
        Hive::LocalMachine | Hive::ClassesRoot | Hive::CurrentConfig | Hive::Users
    )
}

/// Check every step against the rules of a live write, touching nothing.
///
/// # Errors
///
/// Returns [`RecordError::Refused`] when the consent is missing, when a
/// protected hive has no second consent, when a step names a key outside
/// `base`, or when a step would delete a whole hive, and
/// [`RecordError::Unsupported`] for a remote base or a hive that takes no
/// writes.
pub fn check_write(base: &RegistrySpec, steps: &[LiveStep], consent: WriteConsent) -> Result<()> {
    if base.is_remote() {
        return Err(RecordError::unsupported(REMOTE_REASON));
    }
    if base.hive == Hive::PerformanceData {
        return Err(RecordError::unsupported(
            "HKEY_PERFORMANCE_DATA takes no writes",
        ));
    }
    if !consent.confirmed {
        return Err(RecordError::refused(
            "the writes to the live registry were not confirmed",
        ));
    }
    if is_protected_hive(base.hive) && !consent.protected_hive_confirmed {
        return Err(RecordError::refused(format!(
            "writes to {} need a second confirmation",
            base.hive.full_name()
        )));
    }
    let root = base.key_path();
    for step in steps {
        let path = step.key_path();
        if !is_within(path, &root) {
            return Err(RecordError::refused(format!(
                "{path} is outside the base key {root}"
            )));
        }
        if matches!(step, LiveStep::DeleteKey { .. }) && sub_key_of(base.hive, path).is_empty() {
            return Err(RecordError::refused(format!(
                "deleting the hive {path} is not allowed"
            )));
        }
    }
    Ok(())
}

/// Carry out `steps` on the live registry, in order, after [`check_write`]
/// accepts all of them.
///
/// Nothing is written when the check refuses. A step that fails stops the
/// run; the steps before it stay written.
///
/// # Errors
///
/// Returns what [`check_write`] returns, or an error that names the step that
/// failed and how many steps were written before it.
pub fn apply_confirmed(
    base: &RegistrySpec,
    steps: &[LiveStep],
    consent: WriteConsent,
) -> Result<()> {
    check_write(base, steps, consent)?;
    let total = steps.len();
    for (index, step) in steps.iter().enumerate() {
        platform::write(base.hive, step).map_err(|error| RecordError::Io {
            context: format!(
                "step {} of {total} on {} ({index} written)",
                index + 1,
                step.key_path()
            ),
            message: error.to_string(),
        })?;
    }
    Ok(())
}

/// The export that puts back what `steps` change, read before they run.
///
/// Imported after the steps ran, the file restores the keys and values the
/// steps touch: a key the steps create is deleted, a key the steps delete
/// comes back whole, and each value the steps write or delete returns to its
/// earlier data or goes away again.
///
/// # Errors
///
/// Returns the errors of [`export`] for a key that exists and cannot be read.
pub fn backup(base: &RegistrySpec, steps: &[LiveStep], options: &LiveOptions) -> Result<RegFile> {
    let mut out = RegFile::empty(RegFileVersion::V5);
    let mut gone: Vec<String> = Vec::new();
    let mut restored: Vec<String> = Vec::new();
    let mut values: BTreeMap<String, (String, Vec<ValueName>)> = BTreeMap::new();
    let whole = LiveOptions {
        recursive: true,
        ..options.clone()
    };
    let single = LiveOptions {
        recursive: false,
        ..options.clone()
    };
    for step in steps {
        let path = step.key_path();
        let covered = |list: &[String]| list.iter().any(|held| is_within(path, held));
        if covered(&gone) || covered(&restored) {
            continue;
        }
        let spec = spec_for(base, path)?;
        let exists = match export(&spec, &single) {
            Ok(_) => true,
            Err(RecordError::NotFound { .. }) => false,
            Err(other) => return Err(other),
        };
        match step {
            _ if !exists => {
                out.keys.push(deletion(path));
                gone.push(path.to_owned());
            }
            LiveStep::DeleteKey { .. } => {
                let saved = export(&spec, &whole)?;
                out.keys.push(deletion(path));
                out.keys.extend(saved.keys);
                restored.push(path.to_owned());
            }
            LiveStep::CreateKey { .. } => {}
            LiveStep::SetValue { value_name, .. } | LiveStep::DeleteValue { value_name, .. } => {
                values
                    .entry(path.to_ascii_lowercase())
                    .or_insert_with(|| (path.to_owned(), Vec::new()))
                    .1
                    .push(value_name.clone());
            }
        }
    }
    for (path, names) in values.into_values() {
        if gone
            .iter()
            .chain(&restored)
            .any(|held| is_within(&path, held))
        {
            continue;
        }
        let current = export(&spec_for(base, &path)?, &single)?;
        let held = current.keys.into_iter().next();
        let mut entries: Vec<RegEntry> = Vec::new();
        for name in names {
            if entries
                .iter()
                .any(|entry| entry.name.raw().eq_ignore_ascii_case(name.raw()))
            {
                continue;
            }
            let data = held.as_ref().and_then(|block| {
                block
                    .entries
                    .iter()
                    .find(|entry| entry.name.raw().eq_ignore_ascii_case(name.raw()))
                    .and_then(|entry| entry.data.clone())
            });
            entries.push(RegEntry {
                name,
                data,
                source: crate::record::ByteRange::default(),
            });
        }
        out.keys.push(RegKeyBlock {
            path,
            delete: false,
            entries,
            source: crate::record::ByteRange::default(),
        });
    }
    Ok(out)
}

fn deletion(path: &str) -> RegKeyBlock {
    RegKeyBlock {
        path: path.to_owned(),
        delete: true,
        entries: Vec::new(),
        source: crate::record::ByteRange::default(),
    }
}

/// The address of `path` on the machine `base` names.
fn spec_for(base: &RegistrySpec, path: &str) -> Result<RegistrySpec> {
    let mut spec = RegistrySpec::parse(path)?;
    spec.machine.clone_from(&base.machine);
    Ok(spec)
}

/// The part of `path` under its hive, empty for the hive itself.
fn sub_key_of(hive: Hive, path: &str) -> &str {
    let name = hive.full_name();
    match path.get(..name.len()) {
        Some(head) if head.eq_ignore_ascii_case(name) => path
            .get(name.len()..)
            .unwrap_or_default()
            .trim_start_matches('\\'),
        _ => path,
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{Hive, LiveOptions, LiveStep, RecordTree, RegFile, RegistrySpec, Result};
    use crate::error::RecordError;

    const NO_REGISTRY: &str = "this platform has no Windows registry";

    pub(super) fn read(_spec: &RegistrySpec, _options: &LiveOptions) -> Result<RecordTree> {
        Err(RecordError::unsupported(NO_REGISTRY))
    }

    pub(super) fn export(_spec: &RegistrySpec, _options: &LiveOptions) -> Result<RegFile> {
        Err(RecordError::unsupported(NO_REGISTRY))
    }

    pub(super) fn write(_hive: Hive, _step: &LiveStep) -> std::io::Result<()> {
        Err(std::io::Error::other(NO_REGISTRY))
    }
}

#[cfg(windows)]
mod platform {
    use super::{sub_key_of, LiveOptions, LiveStep, RecordTree, RegFile, RegistrySpec, Result};
    use crate::bytes::to_u64;
    use crate::error::RecordError;
    use crate::limits::RecordBudget;
    use crate::record::{compare_names, ByteRange, Record};
    use crate::registry::reg_file::{RegEntry, RegFileVersion, RegKeyBlock};
    use crate::registry::value::{ValueData, ValueKind, ValueName};
    use crate::registry::{Hive, RegistryView};
    use std::borrow::Cow;
    use std::io;
    use winreg::enums::{
        RegType, HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        HKEY_PERFORMANCE_DATA, HKEY_USERS, KEY_ALL_ACCESS, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE,
        KEY_READ, KEY_SET_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY, KEY_WRITE,
    };
    use winreg::{RegKey, RegValue};

    /// Error code the operating system returns for a refused open.
    const ERROR_ACCESS_DENIED: i32 = 5;
    /// Error code the operating system returns for a missing key.
    const ERROR_FILE_NOT_FOUND: i32 = 2;

    pub(super) fn read(spec: &RegistrySpec, options: &LiveOptions) -> Result<RecordTree> {
        let root = RegKey::predef(hive_handle(spec.hive));
        let flags = KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | view_flag(&options.view);
        let key = if spec.sub_key.is_empty() {
            root
        } else {
            root.open_subkey_with_flags(&spec.sub_key, flags)
                .map_err(|error| map_io(&error, &spec.key_path()))?
        };
        let mut budget = RecordBudget::new(&options.limits);
        let base = spec.key_path();
        let name = base.rsplit('\\').next().unwrap_or(&base).to_owned();
        let mut tree = RecordTree::new(base.clone(), name);
        tree.records = read_values(&key, &base, &options.limits, &mut budget)?;
        if options.recursive {
            read_children(&key, &mut tree, options, flags, &mut budget, 0)?;
        }
        tree.sort_by_name();
        Ok(tree)
    }

    /// Walk the sub keys with an explicit stack, so a deep key costs heap.
    fn read_children(
        root: &RegKey,
        tree: &mut RecordTree,
        options: &LiveOptions,
        flags: u32,
        budget: &mut RecordBudget,
        depth: u32,
    ) -> Result<()> {
        options.limits.check_depth(depth)?;
        // A key that vanishes or refuses enumeration mid walk leaves the rest
        // of the walk usable, so it is skipped rather than fatal.
        let names: Vec<String> = root.enum_keys().flatten().collect();
        for name in names {
            options.limits.check_name(to_u64(name.len()))?;
            let Ok(child_key) = root.open_subkey_with_flags(&name, flags) else {
                continue;
            };
            let path = format!("{}\\{}", tree.path, name);
            let mut child = RecordTree::new(path.clone(), name);
            child.records = read_values(&child_key, &path, &options.limits, budget)?;
            read_children(&child_key, &mut child, options, flags, budget, depth + 1)?;
            tree.push_child(child);
        }
        Ok(())
    }

    fn read_values(
        key: &RegKey,
        path: &str,
        limits: &crate::limits::Limits,
        budget: &mut RecordBudget,
    ) -> Result<Vec<Record>> {
        let mut out = Vec::new();
        for entry in key.enum_values() {
            let Ok((name, raw)) = entry else {
                continue;
            };
            budget.spend()?;
            limits.check_name(to_u64(name.len()))?;
            let kind = ValueKind::from_code(reg_type_code(&raw.vtype));
            let data = ValueData::from_raw(kind, raw.bytes.as_ref(), limits)?;
            let display = data.to_display();
            out.push(
                Record::new(
                    path.to_owned(),
                    ValueName::from_raw(&name).display().to_owned(),
                    data.kind().name(),
                    data.to_record_value(),
                )
                .with_display(display),
            );
        }
        Ok(out)
    }

    pub(super) fn export(spec: &RegistrySpec, options: &LiveOptions) -> Result<RegFile> {
        let root = RegKey::predef(hive_handle(spec.hive));
        let flags = KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | view_flag(&options.view);
        let open = |sub: &str| {
            if sub.is_empty() {
                Ok(RegKey::predef(hive_handle(spec.hive)))
            } else {
                root.open_subkey_with_flags(sub, flags)
            }
        };
        let base = open(&spec.sub_key).map_err(|error| map_io(&error, &spec.key_path()))?;
        drop(base);
        let mut budget = RecordBudget::new(&options.limits);
        let mut out = RegFile::empty(RegFileVersion::V5);
        // Sub key paths wait on the stack rather than open handles, so a key
        // with many children holds one handle at a time.
        let mut stack: Vec<(String, String, u32)> =
            vec![(spec.sub_key.clone(), spec.key_path(), 0)];
        while let Some((sub, path, depth)) = stack.pop() {
            // A key that vanishes mid walk leaves the rest usable; only the
            // base key has to open.
            let Ok(key) = open(&sub) else {
                continue;
            };
            let entries = read_entries(&key, &options.limits, &mut budget)?;
            out.keys.push(RegKeyBlock {
                path: path.clone(),
                delete: false,
                entries,
                source: ByteRange::default(),
            });
            if !options.recursive {
                break;
            }
            options.limits.check_depth(depth)?;
            let mut names: Vec<String> = key.enum_keys().flatten().collect();
            names.sort_by(|a, b| compare_names(a, b));
            for name in names.into_iter().rev() {
                options.limits.check_name(to_u64(name.len()))?;
                let child_sub = if sub.is_empty() {
                    name.clone()
                } else {
                    format!("{sub}\\{name}")
                };
                stack.push((child_sub, format!("{path}\\{name}"), depth + 1));
            }
        }
        Ok(out)
    }

    fn read_entries(
        key: &RegKey,
        limits: &crate::limits::Limits,
        budget: &mut RecordBudget,
    ) -> Result<Vec<RegEntry>> {
        let mut out = Vec::new();
        for entry in key.enum_values() {
            let Ok((name, raw)) = entry else {
                continue;
            };
            budget.spend()?;
            limits.check_name(to_u64(name.len()))?;
            let kind = ValueKind::from_code(reg_type_code(&raw.vtype));
            let data = ValueData::from_raw(kind, raw.bytes.as_ref(), limits)?;
            out.push(RegEntry {
                name: ValueName::from_raw(&name),
                data: Some(data),
                source: ByteRange::default(),
            });
        }
        out.sort_by(|a, b| compare_names(a.name.raw(), b.name.raw()));
        Ok(out)
    }

    pub(super) fn write(hive: Hive, step: &LiveStep) -> io::Result<()> {
        let root = RegKey::predef(hive_handle(hive));
        let sub = sub_key_of(hive, step.key_path());
        match step {
            LiveStep::CreateKey { .. } => {
                root.create_subkey_with_flags(sub, KEY_READ | KEY_WRITE)?;
            }
            LiveStep::SetValue {
                value_name, data, ..
            } => {
                let (key, _) = root.create_subkey_with_flags(sub, KEY_SET_VALUE)?;
                let value = RegValue {
                    bytes: Cow::Owned(data.to_raw()),
                    vtype: reg_type(data.kind())?,
                };
                key.set_raw_value(value_name.raw(), &value)?;
            }
            LiveStep::DeleteValue { value_name, .. } => {
                let key = match root.open_subkey_with_flags(sub, KEY_SET_VALUE) {
                    Ok(key) => key,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error),
                };
                match key.delete_value(value_name.raw()) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
            LiveStep::DeleteKey { .. } => {
                // An empty name makes the tree delete clear the parent itself,
                // so a step that names no key below the hive never gets here.
                let (parent, leaf) = sub.rsplit_once('\\').unwrap_or(("", sub));
                if leaf.is_empty() {
                    return Err(io::Error::other("a hive cannot be deleted"));
                }
                let holder = if parent.is_empty() {
                    root
                } else {
                    match root.open_subkey_with_flags(parent, KEY_ALL_ACCESS) {
                        Ok(key) => key,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                        Err(error) => return Err(error),
                    }
                };
                match holder.delete_subkey_all(leaf) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn reg_type(kind: ValueKind) -> io::Result<RegType> {
        Ok(match kind {
            ValueKind::None => RegType::REG_NONE,
            ValueKind::Sz => RegType::REG_SZ,
            ValueKind::ExpandSz => RegType::REG_EXPAND_SZ,
            ValueKind::Binary => RegType::REG_BINARY,
            ValueKind::Dword => RegType::REG_DWORD,
            ValueKind::DwordBigEndian => RegType::REG_DWORD_BIG_ENDIAN,
            ValueKind::Link => RegType::REG_LINK,
            ValueKind::MultiSz => RegType::REG_MULTI_SZ,
            ValueKind::ResourceList => RegType::REG_RESOURCE_LIST,
            ValueKind::FullResourceDescriptor => RegType::REG_FULL_RESOURCE_DESCRIPTOR,
            ValueKind::ResourceRequirementsList => RegType::REG_RESOURCE_REQUIREMENTS_LIST,
            ValueKind::Qword => RegType::REG_QWORD,
            ValueKind::Other(code) => {
                return Err(io::Error::other(format!(
                    "the value type code {code} cannot be written by this build"
                )))
            }
        })
    }

    const fn hive_handle(hive: Hive) -> winreg::HKEY {
        match hive {
            Hive::ClassesRoot => HKEY_CLASSES_ROOT,
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
            Hive::Users => HKEY_USERS,
            Hive::CurrentConfig => HKEY_CURRENT_CONFIG,
            Hive::PerformanceData => HKEY_PERFORMANCE_DATA,
        }
    }

    const fn view_flag(view: &RegistryView) -> u32 {
        match view {
            RegistryView::Bit32 => KEY_WOW64_32KEY,
            RegistryView::Bit64 => KEY_WOW64_64KEY,
            _ => 0,
        }
    }

    fn reg_type_code(vtype: &RegType) -> u32 {
        match vtype {
            RegType::REG_NONE => 0,
            RegType::REG_SZ => 1,
            RegType::REG_EXPAND_SZ => 2,
            RegType::REG_BINARY => 3,
            RegType::REG_DWORD => 4,
            RegType::REG_DWORD_BIG_ENDIAN => 5,
            RegType::REG_LINK => 6,
            RegType::REG_MULTI_SZ => 7,
            RegType::REG_RESOURCE_LIST => 8,
            RegType::REG_FULL_RESOURCE_DESCRIPTOR => 9,
            RegType::REG_RESOURCE_REQUIREMENTS_LIST => 10,
            RegType::REG_QWORD => 11,
        }
    }

    fn map_io(error: &io::Error, path: &str) -> RecordError {
        match error.raw_os_error() {
            Some(ERROR_ACCESS_DENIED) => RecordError::AccessDenied {
                path: path.to_owned(),
            },
            Some(ERROR_FILE_NOT_FOUND) => RecordError::NotFound {
                path: path.to_owned(),
            },
            _ => RecordError::Io {
                context: format!("open registry key {path}"),
                message: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{check_write, is_protected_hive, sub_key_of, WriteConsent};
    use crate::registry::plan::LiveStep;
    use crate::registry::{Hive, RegistrySpec};
    use crate::RecordError;

    const BASE: &str = r"reg:\\HKEY_CURRENT_USER\Software\compare-all-tests\base";

    fn yes() -> WriteConsent {
        WriteConsent {
            confirmed: true,
            protected_hive_confirmed: false,
        }
    }

    fn create(path: &str) -> LiveStep {
        LiveStep::CreateKey {
            key_path: path.to_owned(),
        }
    }

    #[test]
    fn a_step_inside_the_base_key_passes_and_one_outside_is_refused() {
        let base = RegistrySpec::parse(BASE).unwrap();
        let inside = create(r"HKEY_CURRENT_USER\Software\compare-all-tests\base\child");
        assert!(check_write(&base, std::slice::from_ref(&inside), yes()).is_ok());
        let sibling = create(r"HKEY_CURRENT_USER\Software\compare-all-tests\baseline");
        let error = check_write(&base, &[inside, sibling], yes()).unwrap_err();
        assert!(matches!(error, RecordError::Refused(reason) if reason.contains("outside")));
    }

    #[test]
    fn a_write_without_consent_is_refused() {
        let base = RegistrySpec::parse(BASE).unwrap();
        let steps = [create(r"HKEY_CURRENT_USER\Software\compare-all-tests\base")];
        assert!(matches!(
            check_write(&base, &steps, WriteConsent::default()),
            Err(RecordError::Refused(_))
        ));
    }

    #[test]
    fn a_protected_hive_needs_the_second_consent() {
        assert!(is_protected_hive(Hive::LocalMachine));
        assert!(is_protected_hive(Hive::ClassesRoot));
        assert!(!is_protected_hive(Hive::CurrentUser));
        let base = RegistrySpec::parse(r"HKLM\Software\Example").unwrap();
        let steps = [create(r"HKEY_LOCAL_MACHINE\Software\Example\X")];
        assert!(matches!(
            check_write(&base, &steps, yes()),
            Err(RecordError::Refused(reason)) if reason.contains("second")
        ));
        let both = WriteConsent {
            confirmed: true,
            protected_hive_confirmed: true,
        };
        assert!(check_write(&base, &steps, both).is_ok());
    }

    #[test]
    fn deleting_a_hive_and_writing_to_a_remote_base_are_refused() {
        let hive = RegistrySpec::parse(r"HKEY_CURRENT_USER").unwrap();
        let steps = [LiveStep::DeleteKey {
            key_path: "HKEY_CURRENT_USER".to_owned(),
        }];
        assert!(matches!(
            check_write(&hive, &steps, yes()),
            Err(RecordError::Refused(_))
        ));
        let remote = RegistrySpec::parse(r"reg:\\Other\HKEY_CURRENT_USER\X").unwrap();
        assert!(matches!(
            check_write(&remote, &[], yes()),
            Err(RecordError::Unsupported(_))
        ));
    }

    #[test]
    fn the_sub_key_drops_the_hive_name() {
        assert_eq!(
            sub_key_of(Hive::CurrentUser, r"HKEY_CURRENT_USER\Software\X"),
            r"Software\X"
        );
        assert_eq!(sub_key_of(Hive::CurrentUser, "HKEY_CURRENT_USER"), "");
    }
}
