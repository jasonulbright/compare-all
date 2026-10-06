//! Where the application keeps its own files.
//!
//! The settings directory is resolved by the session crate, which owns it. This
//! module adds the folders that hang off it. An environment that names no home
//! yields a folder private to the user for this run, with a notice that says
//! so, rather than a fixed name in a shared folder.
//!
//! `COMPARE_ALL_SETTINGS_DIR` replaces the resolved directory while it holds a
//! non-empty value. An unset variable leaves the result unchanged.

use std::{path::PathBuf, sync::OnceLock};

/// The settings directory and what a person has to be told about it.
struct Resolved {
    directory: PathBuf,
    notice: Option<String>,
}

static RESOLVED: OnceLock<Resolved> = OnceLock::new();

fn resolved() -> &'static Resolved {
    RESOLVED.get_or_init(|| {
        let executable_directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
        let mut notice = None;
        let paths =
            ca_session::SettingsPaths::resolve_with(executable_directory.as_deref(), || {
                let state = ca_session::SettingsPaths::state_directory();
                notice = state.notice;
                state.path
            });
        Resolved {
            directory: paths.directory().path().to_path_buf(),
            notice,
        }
    })
}

/// The base directory for this application's own files.
///
/// Resolved once during startup, before UI frames. The cached choice keeps
/// portable-marker checks off the frame thread and all callers on one directory.
#[must_use]
pub fn settings_directory() -> PathBuf {
    resolved().directory.clone()
}

/// What a person has to be told about the settings directory, such as that
/// settings do not last because no home folder is known.
///
/// None until [`settings_directory`] has been resolved.
#[must_use]
pub fn settings_notice() -> Option<&'static str> {
    RESOLVED
        .get()
        .and_then(|resolved| resolved.notice.as_deref())
}

/// The notice of [`settings_notice`] when `directory` is the resolved settings
/// directory.
#[must_use]
pub fn settings_notice_for(directory: &std::path::Path) -> Option<&'static str> {
    RESOLVED
        .get()
        .filter(|resolved| resolved.directory == directory)
        .and_then(|resolved| resolved.notice.as_deref())
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

/// The folder of this process for copies, created when missing.
///
/// Performs disk I/O; call it on a worker.
///
/// # Errors
///
/// Returns an error when the folder or the shared one above it cannot be
/// created or is not fit for private copies.
pub fn prepared_temporary_directory() -> std::io::Result<PathBuf> {
    prepared_temporary_directory_in(&temporary_root(), process_folder_name())
}

/// The folder `own` under `root`, created when missing.
///
/// On Unix both folders are open only to the running user, and a `root` that
/// another user owns is refused before anything is created in it: that user
/// could read every copy or replace one between its write and its read.
fn prepared_temporary_directory_in(
    root: &std::path::Path,
    own: impl AsRef<std::path::Path>,
) -> std::io::Result<PathBuf> {
    ca_io::private::create_owned_folder(root)?;
    let folder = root.join(own);
    ca_io::private::create_owned_folder(&folder)?;
    Ok(folder)
}

/// Create `folder` as the folder of a process under its shared root, then lock
/// the marker file inside it.
fn claim(folder: &std::path::Path) -> std::io::Result<std::fs::File> {
    let (Some(root), Some(own)) = (folder.parent(), folder.file_name()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} names no folder under a root", folder.display()),
        ));
    };
    prepared_temporary_directory_in(root, own)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join(LOCK_FILE))?;
    file.try_lock().map_err(std::io::Error::from)?;
    Ok(file)
}

/// Delete the folders that earlier runs of this application left under
/// `root`, except `own` and the folders another live process holds locked.
/// Returns how many folders were deleted.
///
/// Only a folder named the way [`process_folder_name`] names one is deleted:
/// the process prefix and a number. A link with that name, and every other
/// item under `root`, stays.
#[must_use]
pub fn sweep_temporary_in(root: &std::path::Path, own: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut deleted = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == own || !is_process_folder_name(&name) {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if is_held(&path) {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// True for the name [`process_folder_name`] gives a process folder.
fn is_process_folder_name(name: &str) -> bool {
    name.strip_prefix(PROCESS_PREFIX)
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
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
        assert_eq!(super::sweep_temporary_in(root.path(), own), 1);
        assert!(root.path().join(own).join("copy").is_dir());
        assert!(live.join(super::LOCK_FILE).is_file());
        assert!(!root.path().join("process-2").exists());
        assert!(root.path().join("from-an-older-build").is_dir());
        assert!(root.path().join("loose.txt").is_file());
        drop(held);
        assert_eq!(super::sweep_temporary_in(root.path(), own), 1);
        assert!(!live.exists());
    }

    #[test]
    fn the_sweep_leaves_every_item_the_application_did_not_make() {
        let root = tempfile::tempdir().unwrap();
        let own = "process-1";
        std::fs::create_dir_all(root.path().join(own)).unwrap();
        std::fs::write(root.path().join("report-2026.docx"), b"user file").unwrap();
        std::fs::create_dir_all(root.path().join("my-notes").join("drafts")).unwrap();
        std::fs::write(
            root.path()
                .join("my-notes")
                .join("drafts")
                .join("draft.txt"),
            b"user draft",
        )
        .unwrap();
        std::fs::write(root.path().join("process-7"), b"a file, not a folder").unwrap();
        std::fs::create_dir_all(root.path().join("process-notes")).unwrap();
        std::fs::write(
            root.path().join("process-notes").join(super::LOCK_FILE),
            b"",
        )
        .unwrap();

        assert_eq!(super::sweep_temporary_in(root.path(), own), 0);
        assert_eq!(
            std::fs::read(root.path().join("report-2026.docx")).unwrap(),
            b"user file"
        );
        assert!(root
            .path()
            .join("my-notes")
            .join("drafts")
            .join("draft.txt")
            .is_file());
        assert!(root.path().join("process-7").is_file());
        assert!(root.path().join("process-notes").is_dir());
    }

    /// The copies hold file content, so the folders above them are open only
    /// to the user who made them.
    #[cfg(unix)]
    #[test]
    fn the_folder_of_a_process_and_its_root_are_open_only_to_their_user() {
        use std::os::unix::fs::PermissionsExt;
        let settings = tempfile::tempdir().unwrap();
        let root = settings.path().join("temporary");
        let own = root.join("process-1");
        let held = super::claim(&own).unwrap();
        for folder in [&root, &own] {
            let mode = std::fs::metadata(folder).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} {mode:o}", folder.display());
        }
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn an_open_root_left_by_an_earlier_build_is_narrowed() {
        use std::os::unix::fs::PermissionsExt;
        let settings = tempfile::tempdir().unwrap();
        let root = settings.path().join("temporary");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        let own = super::prepared_temporary_directory_in(&root, "process-2").unwrap();
        let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{mode:o}");
        let mode = std::fs::metadata(&own).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    /// Only a privileged run can hand a folder to another user, so the case
    /// is checked where the test runs with that privilege.
    #[cfg(unix)]
    #[test]
    fn a_root_of_another_user_is_not_used_when_run_as_root() {
        let settings = tempfile::tempdir().unwrap();
        let root = settings.path().join("temporary");
        std::fs::create_dir(&root).unwrap();
        if std::os::unix::fs::chown(&root, Some(65534), Some(65534)).is_err() {
            println!("skipped: handing a folder to another user needs root");
            return;
        }
        let own = root.join("process-1");
        assert!(super::claim(&own).is_err());
        assert!(!own.exists());
        assert!(super::prepared_temporary_directory_in(&root, "process-1").is_err());
    }

    #[test]
    fn a_root_that_is_a_file_is_not_used() {
        let settings = tempfile::tempdir().unwrap();
        let root = settings.path().join("temporary");
        std::fs::write(&root, b"x").unwrap();
        assert!(super::prepared_temporary_directory_in(&root, "process-1").is_err());
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
