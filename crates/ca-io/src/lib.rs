//! Local file replacement shared by editors, exports, snapshots and settings.
//!
//! Write to an exclusively created sibling, flush and sync it, then replace the
//! destination with an OS replacement operation. Errors before that operation
//! leave the destination untouched. The owned temporary is removed on failure
//! (best effort if the OS refuses cleanup); unrelated siblings are never removed.
//!
//! This is not a power-loss guarantee. Callers own overwrite consent, parent
//! creation and concurrent-change policy. Existing read-only and
//! non-regular targets (including symlinks) are refused at preparation time.
//! Unix permission bits are preserved. Existing Windows files use `ReplaceFileW`;
//! tests cover protected DACLs, the Hidden flag, named streams and creation time.
//! Ownership and other metadata remain unverified. Path races require separate
//! coordination.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

// Serialize the final check and replacement across this process. External
// writers do not participate in this lock.
static COMMIT_LOCK: Mutex<()> = Mutex::new(());

/// Replace a file with `bytes`, keeping its previous contents on failure.
///
/// # Errors
/// Returns an error if the target is unsuitable, or preparation, writing,
/// synchronization or replacement fails. The parent must already exist.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, |writer| writer.write_all(bytes))
}

/// Replace a local state file, writing it owner-only on Unix. Intended for
/// connection trust stores and similar private state.
///
/// # Errors
/// Returns an error if preparation, writing, synchronization or replacement
/// fails. The parent directory must already exist.
pub fn write_atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace_impl(
        path,
        |writer| writer.write_all(bytes),
        || Ok(()),
        |file| {
            file.flush()?;
            file.sync_all()
        },
        0o600,
        false,
        None,
    )
    .map(|(value, _)| value)
}

/// Open a log file for streamed lines. With `append` the existing lines stay;
/// without it the file is first replaced by an empty one through
/// [`write_atomic`]. An existing target that is not a writable regular file
/// (a folder, a link, a read-only file) is refused.
///
/// # Errors
/// Returns an error if the target is unsuitable or cannot be opened. The
/// parent must already exist.
pub fn open_log(path: &Path, append: bool) -> io::Result<File> {
    if append {
        target_permissions(path, false)?;
    } else {
        write_atomic(path, b"")?;
    }
    fs::OpenOptions::new().create(true).append(true).open(path)
}

/// Remove one regular file. A folder, a link or a read-only file is refused
/// and left in place.
///
/// # Errors
/// Returns [`io::ErrorKind::NotFound`] when the file is absent, or another
/// error if the target is unsuitable or the removal fails.
pub fn remove_file(path: &Path) -> io::Result<()> {
    target_permissions(path, false)?;
    fs::remove_file(path)
}

/// Produce a complete file before replacing the destination.
///
/// A producer using a buffered wrapper must explicitly flush it before returning
/// success. Its result is returned only after replacement succeeds.
///
/// # Errors
/// Returns the producer's error or a converted I/O error. The parent must exist.
pub fn replace<T, E: From<io::Error>>(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> Result<T, E>,
) -> Result<T, E> {
    replace_checked(path, produce, || Ok(()))
}

/// Like [`replace`], with a check immediately before each replacement attempt.
///
/// This supports cancellation after writing and during bounded retries for a
/// Windows sharing violation. The check is not atomic with the rename and does
/// not provide compare-and-swap semantics against external writers. In-process
/// replacements serialize their final check and commit; `check` must not start
/// another replacement synchronously.
///
/// # Errors
/// Returns a producer/check error or a converted I/O error before committing.
pub fn replace_checked<T, E: From<io::Error>>(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> Result<T, E>,
    check: impl FnMut() -> Result<(), E>,
) -> Result<T, E> {
    replace_impl(
        path,
        produce,
        check,
        |file| {
            file.flush()?;
            file.sync_all()
        },
        0o666,
        false,
        None,
    )
    .map(|(value, _)| value)
}

/// Like [`replace_checked`], retaining a previous version when `has_conflict`
/// reports that it changed after the final precommit check. On Unix a hard
/// link keeps the previous inode available across the rename. The returned
/// path names the retained previous contents.
///
/// # Errors
/// Returns a producer/check error or a converted I/O error.
pub fn replace_checked_preserving_conflict<T, E: From<io::Error>>(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> Result<T, E>,
    check: impl FnMut() -> Result<(), E>,
    mut has_conflict: impl FnMut(&Path) -> bool,
) -> Result<(T, Option<PathBuf>), E> {
    replace_impl(
        path,
        produce,
        check,
        |file| {
            file.flush()?;
            file.sync_all()
        },
        0o666,
        false,
        Some(&mut has_conflict),
    )
}

/// Write a new file only if the target remains absent at commit time.
///
/// # Errors
/// Returns [`io::ErrorKind::AlreadyExists`] if another writer creates the
/// target before commit, or another preparation, write or sync error.
pub fn replace_new_checked<T, E: From<io::Error>>(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> Result<T, E>,
    check: impl FnMut() -> Result<(), E>,
) -> Result<T, E> {
    replace_impl(
        path,
        produce,
        check,
        |file| {
            file.flush()?;
            file.sync_all()
        },
        0o666,
        true,
        None,
    )
    .map(|(value, _)| value)
}

fn replace_impl<T, E: From<io::Error>>(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> Result<T, E>,
    mut check: impl FnMut() -> Result<(), E>,
    finish: impl FnOnce(&mut File) -> io::Result<()>,
    new_file_mode: u32,
    require_absent: bool,
    mut has_conflict: Option<&mut dyn FnMut(&Path) -> bool>,
) -> Result<(T, Option<PathBuf>), E> {
    #[cfg(windows)]
    let extended_path = windows_extended_path(path)?;
    #[cfg(windows)]
    let path = extended_path.as_deref().unwrap_or(path);
    let permissions = target_permissions(path, require_absent)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".compare-all-");
    #[cfg(unix)]
    if permissions.is_none() {
        use std::os::unix::fs::PermissionsExt;
        // Match File::create for a new general output: mode 0666 under umask.
        builder.permissions(fs::Permissions::from_mode(new_file_mode));
    }
    #[cfg(not(unix))]
    let _ = new_file_mode;
    let mut temporary = builder.tempfile_in(parent)?;
    let result = produce(temporary.as_file_mut())?;
    if let Some(permissions) = &permissions {
        #[cfg(unix)]
        let permissions = if new_file_mode == 0o600 {
            use std::os::unix::fs::PermissionsExt;
            fs::Permissions::from_mode(0o600)
        } else {
            permissions.clone()
        };
        #[cfg(not(unix))]
        let permissions = permissions.clone();
        temporary.as_file().set_permissions(permissions)?;
    }
    finish(temporary.as_file_mut())?;

    // Sharing violations get bounded retries. Never delete the destination
    // to make a replacement succeed.
    let mut backoff = Duration::from_millis(5);
    let mut attempt = 0;
    #[cfg(windows)]
    if permissions.is_some() {
        // ReplaceFile must open the replacement itself. Close our writing
        // handle first, while retaining ownership of the temporary path.
        let replacement = temporary.into_temp_path();
        loop {
            let guard = COMMIT_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            check()?;
            let outcome = replace_existing_windows(path, &replacement, parent);
            drop(guard);
            match outcome {
                Ok(mut backup) => {
                    let retained = retain_conflict_backup(&mut backup, &mut has_conflict);
                    sync_directory(parent);
                    return Ok((result, retained));
                }
                Err(error) if retryable(&error) && attempt < 7 => {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(200));
                    attempt += 1;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    loop {
        let guard = COMMIT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        check()?;
        #[cfg(unix)]
        let mut backup = if has_conflict.is_some() && permissions.is_some() {
            link_unix_backup(path, parent)?
        } else {
            None
        };
        #[cfg(not(unix))]
        let mut backup: Option<tempfile::TempPath> = None;
        let outcome = if require_absent {
            temporary.persist_noclobber(path)
        } else {
            temporary.persist(path)
        };
        drop(guard);
        match outcome {
            Ok(_) => {
                let retained = backup
                    .as_mut()
                    .and_then(|backup| retain_conflict_backup(backup, &mut has_conflict));
                sync_directory(parent);
                return Ok((result, retained));
            }
            Err(error) => {
                if !retryable(&error.error) || attempt == 7 {
                    return Err(error.error.into());
                }
                temporary = error.file;
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_millis(200));
                attempt += 1;
            }
        }
    }
}

fn retain_conflict_backup(
    backup: &mut tempfile::TempPath,
    has_conflict: &mut Option<&mut dyn FnMut(&Path) -> bool>,
) -> Option<PathBuf> {
    if has_conflict.as_mut().is_some_and(|check| check(backup)) {
        let path = backup.to_path_buf();
        backup.disable_cleanup(true);
        Some(path)
    } else {
        None
    }
}

#[cfg(unix)]
fn link_unix_backup(path: &Path, parent: &Path) -> io::Result<Option<tempfile::TempPath>> {
    link_unix_backup_with(path, parent, |source, backup| fs::hard_link(source, backup))
}

#[cfg(unix)]
fn link_unix_backup_with(
    path: &Path,
    parent: &Path,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<Option<tempfile::TempPath>> {
    let reserved = tempfile::Builder::new()
        .prefix(".compare-all-backup-")
        .tempfile_in(parent)?
        .into_temp_path();
    let backup_name = reserved.to_path_buf();
    reserved.close()?;
    if let Err(error) = link(path, &backup_name) {
        // Some volumes allow rename but not hard links. Keep saving there,
        // without the additional displaced-file snapshot.
        // macOS ENOTSUP (45) is not classified as Unsupported by Rust 1.94.
        if matches!(
            error.kind(),
            io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
        ) || cfg!(target_os = "macos") && error.raw_os_error() == Some(45)
        {
            return Ok(None);
        }
        return Err(error);
    }
    tempfile::TempPath::try_from_path(&backup_name)
        .inspect_err(|_| {
            let _ = fs::remove_file(&backup_name);
        })
        .map(Some)
}

fn target_permissions(path: &Path, require_absent: bool) -> io::Result<Option<fs::Permissions>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if require_absent {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "target appeared",
                ));
            }
            if !metadata.is_file() || metadata.permissions().readonly() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "replacement requires a writable regular file",
                ));
            }
            Ok(Some(metadata.permissions()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Interrupted
    ) || cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))
}

#[cfg(windows)]
fn replace_existing_windows(
    path: &Path,
    replacement: &Path,
    parent: &Path,
) -> io::Result<tempfile::TempPath> {
    // A backup is essential: Windows error 1176 can otherwise remove the target
    // name, while 1177 can move the original to the backup name. Keep a reserved
    // random sibling so no two writers claim the same recovery path.
    let mut backup = tempfile::Builder::new()
        .prefix(".compare-all-backup-")
        .tempfile_in(parent)?
        .into_temp_path();
    let target_name = stored_spelling(&windows_path(path)?, path);
    let replacement_name = windows_path(replacement)?;
    let backup_name = windows_path(&backup)?;
    match winsafe::ReplaceFile(
        &target_name,
        &replacement_name,
        Some(&backup_name),
        winsafe::co::REPLACEFILE::default(),
    ) {
        Ok(()) => Ok(backup),
        Err(code) => Err(recover_windows_failure(path, &mut backup, code)),
    }
}

/// The target path with its final component spelled as the directory stores
/// it. `ReplaceFileW` gives the replaced file the caller's spelling, so a
/// path that differs in letter case, or names the short 8.3 alias, would
/// otherwise rename the file. A Windows file name holds no wildcard, so the
/// search matches only the target itself.
#[cfg(windows)]
fn stored_spelling(target_name: &str, path: &Path) -> String {
    use winsafe::prelude::kernel_Hfindfile;
    let mut found = winsafe::WIN32_FIND_DATA::default();
    let stored = match winsafe::HFINDFILE::FindFirstFile(target_name, &mut found) {
        Ok((_guard, true)) => found.cFileName(),
        _ => return target_name.to_owned(),
    };
    match (
        path.file_name().and_then(|name| name.to_str()),
        path.parent(),
    ) {
        (Some(name), Some(parent)) if name != stored && !stored.is_empty() => {
            windows_path(&parent.join(stored)).unwrap_or_else(|_| target_name.to_owned())
        }
        _ => target_name.to_owned(),
    }
}

#[cfg(windows)]
fn recover_windows_failure(
    path: &Path,
    backup: &mut tempfile::TempPath,
    code: winsafe::co::ERROR,
) -> io::Error {
    let source = io::Error::from_raw_os_error(code.raw().cast_signed());
    if code != winsafe::co::ERROR::UNABLE_TO_MOVE_REPLACEMENT_2 {
        return source;
    }
    // For 1177 the original may now exist only at the backup path. Restore
    // its name when free. Otherwise keep the backup and tell the caller where.
    if fs::symlink_metadata(path).is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
        && fs::rename(&*backup, path).is_ok()
    {
        return source;
    }
    if backup.exists() {
        backup.disable_cleanup(true);
        io::Error::other(format!(
            "{source}; previous contents kept at {}",
            backup.display()
        ))
    } else {
        io::Error::other(format!(
            "{source}; the backup expected at {} is missing",
            backup.display()
        ))
    }
}

#[cfg(windows)]
fn windows_path(path: &Path) -> io::Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the Windows replacement API requires a Unicode path",
        )
    })
}

#[cfg(windows)]
fn windows_extended_path(path: &Path) -> io::Result<Option<PathBuf>> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    const BACKSLASH: u16 = 92;

    // tempfile and ReplaceFileW pass paths through without std's long-path
    // conversion. Give both the target and its sibling temporaries a verbatim
    // parent before the ordinary Win32 limit can be reached.
    let absolute = std::path::absolute(path)?;
    let wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if wide.starts_with(&[BACKSLASH, BACKSLASH, u16::from(b'?'), BACKSLASH])
        || wide.starts_with(&[BACKSLASH, BACKSLASH, u16::from(b'.'), BACKSLASH])
    {
        return Ok(None);
    }
    // Backup names are longer than some targets. Include room for the longest
    // sibling name as well as the target itself before choosing a plain path.
    let parent_len = absolute.parent().map_or(wide.len(), |parent| {
        parent.as_os_str().encode_wide().count()
    });
    if wide.len() < 240 && parent_len + 1 + 32 < 260 {
        return Ok(None);
    }
    let (prefix, tail) = if wide.starts_with(&[BACKSLASH, BACKSLASH]) {
        (r"\\?\UNC\", &wide[2..])
    } else {
        (r"\\?\", wide.as_slice())
    };
    let mut extended: Vec<u16> = prefix.encode_utf16().collect();
    extended.extend_from_slice(tail);
    Ok(Some(PathBuf::from(OsString::from_wide(&extended))))
}

// Best effort after commit: reporting an error here would misleadingly imply
// the old destination was still present. Do not promise power-loss durability.
#[cfg(not(windows))]
fn sync_directory(parent: &Path) {
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(windows)]
fn sync_directory(_parent: &Path) {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
