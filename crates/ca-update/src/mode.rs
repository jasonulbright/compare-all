//! Installed (MSI) or portable (zip).
//!
//! The installer writes `InstallFolder` under `Software\compare-all` in
//! `HKEY_LOCAL_MACHINE` (install for every user) or `HKEY_CURRENT_USER`
//! (install for one user). A running program is an installed copy when that
//! folder is the folder of its executable. Every other copy, including a copy
//! beside a stale registry entry, is portable.

use std::path::Path;

/// How the running program was put on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Installed by the MSI. `machine` is true for an install for every user.
    Installed {
        /// True when the install is per machine.
        machine: bool,
    },
    /// Unpacked from the zip.
    Portable,
}

fn normalize(path: &str) -> String {
    let path = path.trim().trim_start_matches(r"\\?\");
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Decide the mode from the folder of the executable and the install folders
/// the registry holds.
#[must_use]
pub fn classify(
    executable_dir: &Path,
    machine_folder: Option<&str>,
    user_folder: Option<&str>,
) -> InstallMode {
    let here = normalize(&executable_dir.to_string_lossy());
    let same = |folder: Option<&str>| folder.is_some_and(|f| normalize(f) == here);
    if same(machine_folder) {
        InstallMode::Installed { machine: true }
    } else if same(user_folder) {
        InstallMode::Installed { machine: false }
    } else {
        InstallMode::Portable
    }
}

/// The mode of the program at `executable`.
#[must_use]
pub fn detect(executable: &Path) -> InstallMode {
    let Some(dir) = executable.parent() else {
        return InstallMode::Portable;
    };
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
        use winreg::RegKey;
        let read = |root| -> Option<String> {
            RegKey::predef(root)
                .open_subkey_with_flags("Software\\compare-all", KEY_READ | KEY_WOW64_64KEY)
                .ok()?
                .get_value("InstallFolder")
                .ok()
        };
        let machine = read(HKEY_LOCAL_MACHINE);
        let user = read(HKEY_CURRENT_USER);
        classify(dir, machine.as_deref(), user.as_deref())
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        InstallMode::Portable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_folder_decides() {
        let dir = Path::new(r"C:\Program Files\compare-all");
        assert_eq!(
            classify(dir, Some(r"C:\Program Files\compare-all\"), None),
            InstallMode::Installed { machine: true }
        );
        assert_eq!(
            classify(dir, None, Some(r"c:\program files\COMPARE-ALL")),
            InstallMode::Installed { machine: false }
        );
        assert_eq!(
            classify(dir, Some(r"D:\elsewhere"), None),
            InstallMode::Portable
        );
        assert_eq!(classify(dir, None, None), InstallMode::Portable);
    }
}
