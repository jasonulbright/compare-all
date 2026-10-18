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

/// The Windows folder. Other platforms have none.
///
/// # Errors
/// Always.
#[cfg(not(windows))]
pub fn system_root() -> Result<std::path::PathBuf, String> {
    Err("this platform has no Windows folder".to_owned())
}
