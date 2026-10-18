//! Locations of operating system programs.

/// The Windows folder, read from the machine registry. The `SystemRoot`
/// variable of the environment is not used: any process of the user can set
/// it for the processes it starts.
///
/// # Errors
/// Returns why the folder could not be read.
#[cfg(windows)]
pub fn system_root() -> Result<std::path::PathBuf, String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let value: String = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
            KEY_READ | KEY_WOW64_64KEY,
        )
        .and_then(|key| key.get_value("SystemRoot"))
        .map_err(|error| format!("cannot read the Windows folder: {error}"))?;
    let path = std::path::PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("the Windows folder is not an absolute path".to_owned())
    }
}

/// The program files folder for 64-bit programs, read from the machine
/// registry for the same reason as [`system_root`].
///
/// # Errors
/// Returns why the folder could not be read.
#[cfg(windows)]
pub fn program_files() -> Result<std::path::PathBuf, String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let value: String = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion",
            KEY_READ | KEY_WOW64_64KEY,
        )
        .and_then(|key| key.get_value("ProgramFilesDir"))
        .map_err(|error| format!("cannot read the program files folder: {error}"))?;
    let path = std::path::PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("the program files folder is not an absolute path".to_owned())
    }
}

/// The profile folders of the accounts of this machine, read from the
/// machine registry, which only administrators can change.
#[cfg(windows)]
#[must_use]
pub fn profile_folders() -> Vec<std::path::PathBuf> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let Ok(list) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(
        "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList",
        KEY_READ | KEY_WOW64_64KEY,
    ) else {
        return Vec::new();
    };
    let root = system_root().ok();
    list.enum_keys()
        .flatten()
        .filter_map(|name| list.open_subkey_with_flags(name, KEY_READ).ok())
        .filter_map(|key| key.get_value::<String, _>("ProfileImagePath").ok())
        .filter_map(|value| expand_system_folders(&value, root.as_deref()))
        .collect()
}

/// `value` with `%SystemRoot%` and `%SystemDrive%` replaced, the only
/// variables profile paths use. A value with any other variable gives
/// `None`.
#[must_use]
pub fn expand_system_folders(
    value: &str,
    root: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let mut text = value.to_owned();
    if text.contains('%') {
        let root = root?.to_string_lossy().into_owned();
        let drive = root.get(..2)?.to_owned();
        for (name, with) in [("%systemroot%", &root), ("%systemdrive%", &drive)] {
            while let Some(at) = text.to_ascii_lowercase().find(name) {
                text.replace_range(at..at + name.len(), with);
            }
        }
        if text.contains('%') {
            return None;
        }
    }
    let path = std::path::PathBuf::from(text);
    path.is_absolute().then_some(path)
}

/// The profile folders of the accounts. Other platforms have none here.
#[cfg(not(windows))]
#[must_use]
pub fn profile_folders() -> Vec<std::path::PathBuf> {
    Vec::new()
}

/// The program files folder. Other platforms have none.
///
/// # Errors
/// Always.
#[cfg(not(windows))]
pub fn program_files() -> Result<std::path::PathBuf, String> {
    Err("this platform has no program files folder".to_owned())
}

/// The Windows folder. Other platforms have none.
///
/// # Errors
/// Always.
#[cfg(not(windows))]
pub fn system_root() -> Result<std::path::PathBuf, String> {
    Err("this platform has no Windows folder".to_owned())
}
