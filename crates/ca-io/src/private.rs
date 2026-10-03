//! Folders only the running user can read.

use std::io;
use std::path::{Path, PathBuf};

/// Create `folder` if it is missing, then check that it is a folder of the
/// running user and, on Unix, narrow its access to that user.
///
/// Missing parents are created with default permissions. An existing link is
/// refused, so a link planted at `folder` cannot send the contents elsewhere.
///
/// # Errors
/// Returns an error when the folder cannot be created, is a link, is not a
/// folder, or belongs to another user.
pub fn create_owned_folder(folder: &Path) -> io::Result<()> {
    if let Some(parent) = folder
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    match builder.create(folder) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    if std::fs::symlink_metadata(folder)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is a link", folder.display()),
        ));
    }
    check_owned_folder(folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(folder)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Check that `folder` is a folder and, on Unix, that the running user owns
/// it.
///
/// A link is followed, so a link to a folder of another user is refused.
///
/// # Errors
/// Returns an error when the folder cannot be read, is not a folder, or
/// belongs to another user.
pub fn check_owned_folder(folder: &Path) -> io::Result<()> {
    let metadata = std::fs::metadata(folder)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a folder", folder.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} belongs to another user", folder.display()),
            ));
        }
    }
    Ok(())
}

/// Check that `folder` is a folder the running user owns and, on Unix, that
/// no group and no other user has any access to it.
///
/// # Errors
/// Returns an error when the folder cannot be read, is not a folder, belongs
/// to another user, or is open to others.
pub fn check_private_folder(folder: &Path) -> io::Result<()> {
    check_owned_folder(folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::metadata(folder)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is open to other users", folder.display()),
            ));
        }
    }
    Ok(())
}

/// True when a new file can be created in `folder`.
///
/// The probe file has no name that stays behind: it is deleted when it
/// closes, so the probe leaves the folder as it was.
#[must_use]
pub fn accepts_new_files(folder: &Path) -> bool {
    tempfile::tempfile_in(folder).is_ok()
}

/// Create a new folder with an unpredictable name under `parent`, open only
/// to the running user on Unix. The folder stays after the call.
///
/// # Errors
/// Returns an error when the folder cannot be created.
pub fn create_private_folder_in(parent: &Path, prefix: &str) -> io::Result<PathBuf> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    Ok(builder.tempdir_in(parent)?.keep())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{check_owned_folder, create_owned_folder, create_private_folder_in};

    #[test]
    fn a_private_folder_has_a_new_name_each_time() {
        let parent = tempfile::tempdir().unwrap();
        let first = create_private_folder_in(parent.path(), "state-").unwrap();
        let second = create_private_folder_in(parent.path(), "state-").unwrap();
        assert_ne!(first, second);
        assert!(first.is_dir() && second.is_dir());
        assert!(first.starts_with(parent.path()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&first).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        }
    }

    #[test]
    fn a_file_is_not_an_owned_folder() {
        let parent = tempfile::tempdir().unwrap();
        let file = parent.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(check_owned_folder(&file).is_err());
        assert!(create_owned_folder(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_not_an_owned_folder() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let error = create_owned_folder(&link).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_open_to_others_is_not_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let folder = parent.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        for (mode, private) in [
            (0o700, true),
            (0o750, false),
            (0o701, false),
            (0o1777, false),
        ] {
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                super::check_private_folder(&folder).is_ok(),
                private,
                "{mode:o}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_folder_is_narrowed_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let folder = parent.path().join("shared");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_owned_folder(&folder).unwrap();
        let mode = std::fs::metadata(&folder).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    /// Only a privileged run can hand a folder to another user, so the case
    /// is checked where the test runs with that privilege.
    #[cfg(unix)]
    #[test]
    fn a_folder_of_another_user_is_refused_when_run_as_root() {
        if rustix::process::geteuid().as_raw() != 0 {
            println!("skipped: handing a folder to another user needs root");
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        let folder = parent.path().join("foreign");
        std::fs::create_dir(&folder).unwrap();
        std::os::unix::fs::chown(&folder, Some(65534), Some(65534)).unwrap();
        let error = check_owned_folder(&folder).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(create_owned_folder(&folder).is_err());
    }
}
