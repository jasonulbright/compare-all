//! Folders only the running user can read.

use std::io;
use std::path::{Path, PathBuf};

/// Create `folder` if it is missing, then check that it is a folder of the
/// running user and, on Unix, narrow its access to that user. Returns the path
/// to use for the folder from then on.
///
/// Missing parents are created with default permissions. On Unix every check
/// and the narrowing act on the opened folder, so a swap of the path after the
/// open changes nothing.
///
/// On Unix an existing link is accepted only when the running user owns the
/// link and its target is a folder of the running user that no group and no
/// other user can open. The target is left as it is, and the returned path is
/// the resolved target, so a later change of the link does not move the
/// folder. Every other link is refused.
///
/// Windows cannot read the owner or the access list of a target without
/// unsafe code, so a link or junction at `folder` is always refused there.
/// For a folder that is not a link, the returned path is `folder`.
///
/// # Errors
/// Returns an error when the folder cannot be created, is not a folder, or
/// belongs to another user. On Unix it also returns an error for a link that
/// another user owns or whose target is not a private folder of the running
/// user; on Windows, for every link.
pub fn create_owned_folder(folder: &Path) -> io::Result<PathBuf> {
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
    #[cfg(unix)]
    {
        adopt_folder(folder, || {})
    }
    #[cfg(not(unix))]
    {
        if std::fs::symlink_metadata(folder)?.file_type().is_symlink() {
            return Err(link_refused(folder));
        }
        check_owned_folder(folder)?;
        Ok(folder.to_path_buf())
    }
}

fn link_refused(folder: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} is a link", folder.display()),
    )
}

/// Open `folder` as a folder. With `follow` false a link at `folder` is not
/// followed and the open fails.
#[cfg(unix)]
fn open_folder(folder: &Path, follow: bool) -> io::Result<std::fs::File> {
    use rustix::fs::{openat, Mode, OFlags, CWD};
    let mut flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    if !follow {
        flags |= OFlags::NOFOLLOW;
    }
    Ok(std::fs::File::from(openat(
        CWD,
        folder,
        flags,
        Mode::empty(),
    )?))
}

/// Check the opened folder `handle`, which was opened from `folder`: it must
/// belong to the running user and, when `private`, be closed to others.
#[cfg(unix)]
fn check_handle(handle: &std::fs::File, folder: &Path, private: bool) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = handle.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a folder", folder.display()),
        ));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to another user", folder.display()),
        ));
    }
    if private && metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is open to other users", folder.display()),
        ));
    }
    Ok(())
}

/// Check the folder at `folder` as a folder of the running user and narrow it
/// to that user, all on the opened folder. `between` runs after the open and
/// before the first check. Returns the path to use for the folder.
#[cfg(unix)]
fn adopt_folder(folder: &Path, between: impl FnOnce()) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let handle = match open_folder(folder, false) {
        Ok(handle) => handle,
        Err(error) => {
            return match std::fs::symlink_metadata(folder) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    accept_linked_folder(folder, &metadata)
                }
                _ => Err(error),
            };
        }
    };
    between();
    check_handle(&handle, folder, false)?;
    if handle.metadata()?.permissions().mode() & 0o077 != 0 {
        handle.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(folder.to_path_buf())
}

/// Accept the link at `folder`, whose own metadata is `link`, only when the
/// running user owns it and its target is a private folder of that user.
/// Returns the resolved target. The target is not changed.
///
/// The returned path holds no link, so a caller that resolves it again does
/// not follow a link that was changed after this check.
#[cfg(unix)]
fn accept_linked_folder(folder: &Path, link: &std::fs::Metadata) -> io::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt as _;
    if link.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is a link of another user", folder.display()),
        ));
    }
    let target = std::fs::canonicalize(folder).map_err(|_| link_refused(folder))?;
    let handle = open_folder(&target, false).map_err(|_| link_refused(folder))?;
    check_handle(&handle, folder, true).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "{} is a link to a folder that is not private to you",
                folder.display()
            ),
        )
    })?;
    Ok(target)
}

/// Check that `folder` is a folder and, on Unix, that the running user owns
/// it.
///
/// A link is followed, so a link to a folder of another user is refused. On
/// Unix the checks act on the opened folder.
///
/// # Errors
/// Returns an error when the folder cannot be read, is not a folder, or
/// belongs to another user.
pub fn check_owned_folder(folder: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        check_handle(&open_folder(folder, true)?, folder, false)
    }
    #[cfg(not(unix))]
    {
        if std::fs::metadata(folder)?.is_dir() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a folder", folder.display()),
            ))
        }
    }
}

/// Check that `folder` is a folder the running user owns and, on Unix, that
/// no group and no other user has any access to it. On Unix the checks act on
/// the opened folder.
///
/// # Errors
/// Returns an error when the folder cannot be read, is not a folder, belongs
/// to another user, or is open to others.
pub fn check_private_folder(folder: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        check_handle(&open_folder(folder, true)?, folder, true)
    }
    #[cfg(not(unix))]
    {
        check_owned_folder(folder)
    }
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
    fn a_link_to_a_private_folder_of_the_user_is_accepted() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let used = create_owned_folder(&link).unwrap();
        assert_eq!(used, std::fs::canonicalize(&target).unwrap());
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_change_of_an_accepted_link_does_not_move_the_folder() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let checked = parent.path().join("checked");
        let other = parent.path().join("other");
        std::fs::create_dir(&checked).unwrap();
        std::fs::set_permissions(&checked, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&checked, &link).unwrap();
        let used = create_owned_folder(&link).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        std::fs::write(used.join("copy"), b"x").unwrap();
        assert!(checked.join("copy").is_file());
        assert!(!other.join("copy").exists());
    }

    /// Only a privileged run can hand a link to another user, so the case is
    /// checked where the test runs with that privilege.
    #[cfg(unix)]
    #[test]
    fn a_link_of_another_user_is_refused_when_run_as_root() {
        use std::os::unix::fs::PermissionsExt as _;
        if rustix::process::geteuid().as_raw() != 0 {
            println!("skipped: handing a link to another user needs root");
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        std::os::unix::fs::lchown(&link, Some(65534), Some(65534)).unwrap();
        let error = create_owned_folder(&link).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_anything_else_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let open = parent.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = parent.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let cases = [
            ("open", open),
            ("file", file),
            ("missing", parent.path().join("missing")),
        ];
        for (name, target) in cases {
            let link = parent.path().join(format!("link-{name}"));
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let error = create_owned_folder(&link).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied, "{name}");
        }
        let mode = std::fs::metadata(parent.path().join("open"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "the target of a refused link is left alone"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_swap_of_the_path_after_the_open_does_not_change_another_folder() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let folder = parent.path().join("folder");
        let other = parent.path().join("other");
        let moved = parent.path().join("moved");
        for path in [&folder, &other] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        super::adopt_folder(&folder, || {
            std::fs::rename(&folder, &moved).unwrap();
            std::os::unix::fs::symlink(&other, &folder).unwrap();
        })
        .unwrap();
        let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode(&moved) & 0o777, 0o700);
        assert_eq!(mode(&other) & 0o777, 0o755);
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
