//! Where the application keeps its own files.
//!
//! The settings directory is resolved by the session crate, which owns it. This
//! module adds the folders that hang off it and a fallback: every write-ahead
//! record has to go somewhere, so an environment that names no home yields the
//! temporary directory rather than leaving a caller without a path.
//!
//! `COMPARE_ALL_SETTINGS_DIR` replaces the resolved directory while it holds a
//! non-empty value. An unset variable leaves the result unchanged.

use std::{path::PathBuf, sync::OnceLock};

/// Folder name used when the environment names no home.
const APPLICATION_FOLDER: &str = "compare-all";

/// The base directory for this application's own files.
///
/// Resolved once during startup, before UI frames. The cached choice keeps
/// portable-marker checks off the frame thread and all callers on one directory.
#[must_use]
pub fn settings_directory() -> PathBuf {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            let per_user = ca_session::SettingsPaths::platform_per_user_directory()
                .unwrap_or_else(|_| std::env::temp_dir().join(APPLICATION_FOLDER));
            let executable_directory = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
            executable_directory.map_or_else(
                || per_user.clone(),
                |directory| {
                    ca_session::SettingsPaths::resolve(&directory, per_user.clone())
                        .directory()
                        .path()
                        .to_path_buf()
                },
            )
        })
        .clone()
}

/// Folder the journals of file operations are written to.
#[must_use]
pub fn journal_directory() -> PathBuf {
    settings_directory().join("journals")
}

/// Folder the copies a tab reads from are made in, such as a file extracted
/// from an archive.
///
/// Each process has a folder of its own under the shared one, so a sweep by
/// one instance never deletes the copies another instance still reads.
#[must_use]
pub fn temporary_directory() -> PathBuf {
    temporary_root().join(process_folder_name())
}

/// The shared folder the folder of each process sits in.
#[must_use]
pub fn temporary_root() -> PathBuf {
    settings_directory().join("temporary")
}

/// Prefix of the folder of one process under [`temporary_root`].
const PROCESS_PREFIX: &str = "process-";
/// File a live process holds locked inside its own temporary folder.
const LOCK_FILE: &str = "lock";

fn process_folder_name() -> String {
    format!("{PROCESS_PREFIX}{}", std::process::id())
}

/// Mark this process's temporary folder as live, then delete what earlier
/// runs left under the shared folder.
///
/// Performs disk I/O; call it on a worker. The lock is held until the process
/// ends, so an instance that starts later leaves this folder alone.
///
/// # Errors
///
/// Returns the error of the first step that fails to create or lock the
/// folder of this process. Leftovers that cannot be deleted are skipped.
pub fn sweep_temporary() -> std::io::Result<usize> {
    static HELD: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();
    let root = temporary_root();
    let own = process_folder_name();
    if HELD.get().is_none() {
        let lock = claim(&root.join(&own))?;
        let _ = HELD.set(lock);
    }
    Ok(sweep_temporary_in(&root, &own))
}

/// Create `folder` and lock the marker file inside it.
fn claim(folder: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(folder)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join(LOCK_FILE))?;
    file.try_lock().map_err(std::io::Error::from)?;
    Ok(file)
}

/// Delete every entry under `root` except `own` and the folders another live
/// process holds locked. Returns how many entries were deleted.
#[must_use]
pub fn sweep_temporary_in(root: &std::path::Path, own: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut deleted = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy() == own {
            continue;
        }
        let path = entry.path();
        let is_process_folder = name.to_string_lossy().starts_with(PROCESS_PREFIX);
        if is_process_folder && is_held(&path) {
            continue;
        }
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if removed.is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// True when a live process holds the lock inside `folder`.
///
/// A folder with no lock file is taken as abandoned. The probe handle is
/// closed before the caller deletes the folder, because an open handle blocks
/// the delete on Windows.
fn is_held(folder: &std::path::Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(folder.join(LOCK_FILE))
    else {
        return false;
    };
    let held = file.try_lock().is_err();
    drop(file);
    held
}

/// The per-user location the running platform would use on its own.
///
/// The override is ignored here, so a caller can compare the two and find out
/// whether a directory is the real one.
#[must_use]
pub fn platform_settings_directory() -> Option<PathBuf> {
    ca_session::SettingsPaths::platform_directory().ok()
}

/// True when `directory` is the platform's real per-user location or sits
/// inside it.
///
/// Tests assert on this: a resolved directory that answers true would put test
/// files among the user's own.
#[must_use]
pub fn is_real_per_user_location(directory: &std::path::Path) -> bool {
    platform_settings_directory().is_some_and(|real| directory.starts_with(real))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{is_real_per_user_location, journal_directory, settings_directory};

    #[test]
    fn the_journal_directory_sits_under_the_settings_directory() {
        assert!(journal_directory().starts_with(settings_directory()));
        assert!(journal_directory().ends_with("journals"));
    }

    #[test]
    fn the_journal_directory_is_absolute() {
        assert!(journal_directory().is_absolute());
    }

    #[test]
    fn the_sweep_deletes_leftovers_and_keeps_live_folders() {
        let root = tempfile::tempdir().unwrap();
        let own = "process-1";
        std::fs::create_dir_all(root.path().join(own).join("copy")).unwrap();
        std::fs::create_dir_all(root.path().join("process-2").join("copy")).unwrap();
        std::fs::create_dir_all(root.path().join("from-an-older-build")).unwrap();
        std::fs::write(root.path().join("loose.txt"), b"x").unwrap();
        let live = root.path().join("process-3");
        let held = super::claim(&live).unwrap();
        assert_eq!(super::sweep_temporary_in(root.path(), own), 3);
        assert!(root.path().join(own).join("copy").is_dir());
        assert!(live.join(super::LOCK_FILE).is_file());
        assert!(!root.path().join("process-2").exists());
        assert!(!root.path().join("loose.txt").exists());
        drop(held);
        assert_eq!(super::sweep_temporary_in(root.path(), own), 1);
        assert!(!live.exists());
    }

    #[test]
    fn the_temporary_folder_of_this_process_sits_under_the_shared_one() {
        let own = super::temporary_directory();
        assert!(own.starts_with(super::temporary_root()));
        assert_ne!(own, super::temporary_root());
    }

    #[test]
    fn the_resolved_settings_directory_is_not_the_real_one() {
        crate::testing::isolate_settings();
        assert!(!is_real_per_user_location(&settings_directory()));
        assert!(!is_real_per_user_location(&journal_directory()));
    }
}
