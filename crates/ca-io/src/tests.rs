#![allow(
    clippy::disallowed_methods,
    reason = "test setup runs a platform tool directly"
)]

use super::*;

fn names(directory: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn creates_and_replaces_complete_files() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    write_atomic(&target, b"long original").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"long original");
    write_atomic(&target, b"new").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(unix)]
#[test]
fn replacement_and_failed_output_keep_the_ordinary_owner_group_and_mode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
    let identity = || {
        let metadata = fs::metadata(&target).unwrap();
        (metadata.uid(), metadata.gid(), metadata.mode())
    };
    let before = identity();
    write_atomic(&target, b"replacement").unwrap();
    assert_eq!(identity(), before);
    let failed = replace(&target, |writer| -> io::Result<()> {
        writer.write_all(b"partial")?;
        Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "injected full volume",
        ))
    });
    assert_eq!(failed.unwrap_err().kind(), io::ErrorKind::StorageFull);
    assert_eq!(identity(), before);
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(windows)]
#[test]
fn replacement_and_failed_output_keep_the_ordinary_windows_owner() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let owner = || {
        let result = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "[System.IO.File]::GetAccessControl($env:COMPARE_ALL_OWNER_TEST_PATH).GetOwner([System.Security.Principal.SecurityIdentifier]).Value"])
            .env("COMPARE_ALL_OWNER_TEST_PATH", &target)
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let owner = String::from_utf8(result.stdout).unwrap();
        let owner = owner.trim().to_owned();
        assert!(owner.starts_with("S-1-"), "unexpected owner: {owner}");
        owner
    };
    let before = owner();
    write_atomic(&target, b"replacement").unwrap();
    assert_eq!(owner(), before);
    let failed = replace(&target, |writer| -> io::Result<()> {
        writer.write_all(b"partial")?;
        Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "injected full volume",
        ))
    });
    assert_eq!(failed.unwrap_err().kind(), io::ErrorKind::StorageFull);
    assert_eq!(owner(), before);
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(windows)]
#[test]
fn a_replacement_through_another_letter_case_keeps_the_stored_name() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Report.TXT"), b"old").unwrap();
    write_atomic(&dir.path().join("report.txt"), b"new").unwrap();
    assert_eq!(names(dir.path()), ["Report.TXT"]);
    assert_eq!(fs::read(dir.path().join("Report.TXT")).unwrap(), b"new");
}

#[cfg(windows)]
#[test]
fn windows_long_paths_create_and_replace() {
    let dir = tempfile::tempdir().unwrap();
    let deep = dir
        .path()
        .join("a".repeat(100))
        .join("b".repeat(100))
        .join("c".repeat(80));
    fs::create_dir_all(&deep).unwrap();
    let target = deep.join("output.txt");
    assert!(target.as_os_str().len() > 260);
    write_atomic(&target, b"created").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"created");
    write_atomic(&target, b"replaced").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"replaced");
    assert_eq!(names(&deep), ["output.txt"]);
}

#[cfg(windows)]
#[test]
fn windows_backup_name_can_cross_the_legacy_path_limit() {
    use std::os::windows::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let root_len = dir.path().as_os_str().encode_wide().count();
    let folder = dir.path().join("d".repeat(239 - root_len - 3));
    fs::create_dir(&folder).unwrap();
    let target = folder.join("a");
    assert_eq!(target.as_os_str().encode_wide().count(), 239);
    fs::write(&target, b"original").unwrap();
    write_atomic(&target, b"replacement").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(names(&folder), ["a"]);
}

#[cfg(windows)]
#[test]
fn windows_long_unc_paths_get_the_unc_verbatim_prefix() {
    let path = format!(r"\\server\share\{}", "folder".repeat(45));
    let extended = windows_extended_path(Path::new(&path)).unwrap().unwrap();
    assert!(extended
        .to_str()
        .unwrap()
        .starts_with(r"\\?\UNC\server\share\"));
}

#[test]
fn partial_write_failure_preserves_old_or_absent_destination_and_other_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    let sibling = dir.path().join(".compare-all-unrelated");
    fs::write(&sibling, b"keep me").unwrap();
    for existing in [false, true] {
        if existing {
            fs::write(&target, b"original").unwrap();
        }
        let before = names(dir.path());
        let result = replace(&target, |writer| -> io::Result<()> {
            writer.write_all(b"partial output")?;
            // The destination must still be untouched while bytes are flowing.
            assert_eq!(target.exists(), existing);
            assert_eq!(names(dir.path()).len(), before.len() + 1);
            Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected disk full",
            ))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::StorageFull);
        assert_eq!(names(dir.path()), before);
        assert_eq!(target.exists(), existing);
        if existing {
            assert_eq!(fs::read(&target).unwrap(), b"original");
        }
        assert_eq!(fs::read(&sibling).unwrap(), b"keep me");
    }
}

#[test]
fn finalization_failure_does_not_commit_or_leave_temporary_files() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    for fail_during_sync in [false, true] {
        let result = replace_impl(
            &target,
            |writer| writer.write_all(b"complete replacement"),
            || Ok(()),
            |file| {
                if fail_during_sync {
                    file.flush()?;
                }
                Err(io::Error::other("injected flush/sync failure"))
            },
            0o666,
            false,
            None,
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(names(dir.path()), ["output"]);
    }
}

#[test]
fn cancellation_after_writing_preserves_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let mut produced = false;
    let result = replace_checked(
        &target,
        |writer| -> io::Result<()> {
            writer.write_all(b"replacement")?;
            produced = true;
            Ok(())
        },
        || Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")),
    );
    assert!(produced);
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(fs::read(&target).unwrap(), b"original");
    assert_eq!(names(dir.path()), ["output"]);
}

#[test]
fn read_only_targets_are_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let original_permissions = fs::metadata(&target).unwrap().permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&target, read_only).unwrap();
    let result = write_atomic(&target, b"replacement");
    let content = fs::read(&target).unwrap();
    fs::set_permissions(&target, original_permissions).unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(content, b"original");
    assert_eq!(names(dir.path()), ["output"]);
}

/// A Windows path drops a dot or a space at the end of its last name.
#[cfg(unix)]
#[test]
fn replaced_content_drops_the_set_user_and_group_id_bits() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("tool.sh");
    fs::write(&target, b"#!/bin/sh\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o6755)).unwrap();
    write_atomic(&target, b"#!/bin/sh\necho replaced\n").unwrap();
    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o755, "{mode:o}");
}

#[cfg(windows)]
#[test]
fn a_name_that_ends_in_a_dot_or_a_space_is_refused_and_the_shorter_name_kept() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("a.txt");
    fs::write(&target, b"original").unwrap();
    for spelling in ["a.txt.", "a.txt "] {
        let refused = dir.path().join(spelling);
        let error = write_atomic(&refused, b"via the other spelling").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        let error = remove_file(&refused).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
    }
    assert_eq!(fs::read(&target).unwrap(), b"original");
}

#[test]
fn directories_are_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("folder");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("child"), b"original").unwrap();
    assert!(write_atomic(&target, b"replacement").is_err());
    assert_eq!(fs::read(target.join("child")).unwrap(), b"original");
    assert_eq!(names(dir.path()), ["folder"]);
}

#[test]
fn one_failed_writer_cannot_remove_another_writers_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    let result = replace(&target, |writer| -> io::Result<()> {
        writer.write_all(b"first, partial")?;
        write_atomic(&target, b"second, complete")?;
        assert_eq!(names(dir.path()).len(), 2);
        Err(io::Error::other("first writer failed"))
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&target).unwrap(), b"second, complete");
    assert_eq!(names(dir.path()), ["output"]);
}

#[test]
fn simultaneous_checked_writers_do_not_both_commit() {
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let ready = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let writers: Vec<_> = [b"first".as_slice(), b"second".as_slice()]
            .into_iter()
            .map(|bytes| {
                let ready = Arc::clone(&ready);
                let target = &target;
                scope.spawn(move || {
                    replace_checked(
                        target,
                        |writer| -> io::Result<()> {
                            writer.write_all(bytes)?;
                            ready.wait();
                            Ok(())
                        },
                        || {
                            if fs::read(target)? == b"original" {
                                Ok(())
                            } else {
                                Err(io::Error::other("another writer committed"))
                            }
                        },
                    )
                })
            })
            .collect();
        writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert!(matches!(
        fs::read(&target).unwrap().as_slice(),
        b"first" | b"second"
    ));
    assert_eq!(names(dir.path()), ["output"]);
}

#[test]
fn new_file_commit_does_not_replace_a_name_created_after_the_check() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    let result = replace_new_checked(
        &target,
        |writer| writer.write_all(b"our contents"),
        || {
            fs::write(&target, b"other writer")?;
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&target).unwrap(), b"other writer");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(unix)]
#[test]
fn unix_save_can_proceed_when_hard_links_are_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    for kind in [io::ErrorKind::Unsupported, io::ErrorKind::PermissionDenied] {
        let backup = link_unix_backup_with(&target, dir.path(), |_, _| Err(kind.into())).unwrap();
        assert!(backup.is_none());
        assert_eq!(names(dir.path()), ["output"]);
    }
    let failure = link_unix_backup_with(&target, dir.path(), |_, _| {
        Err(io::ErrorKind::NotFound.into())
    });
    assert_eq!(failure.unwrap_err().kind(), io::ErrorKind::NotFound);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_enotsup_allows_commit_without_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let backup = link_unix_backup_with(&target, dir.path(), |_, _| {
        Err(io::Error::from_raw_os_error(45))
    })
    .unwrap();
    assert!(backup.is_none());
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(unix)]
#[test]
fn unix_displaced_change_is_retained_after_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let ((), backup) = replace_checked_preserving_conflict(
        &target,
        |writer| writer.write_all(b"our edit"),
        || {
            fs::write(&target, b"other writer")?;
            Ok(())
        },
        |backup| fs::read(backup).unwrap() != b"original",
    )
    .unwrap();
    let backup = backup.unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"our edit");
    assert_eq!(fs::read(&backup).unwrap(), b"other writer");
    fs::remove_file(backup).unwrap();
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(unix)]
#[test]
fn unix_unchanged_displaced_version_is_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let ((), backup) = replace_checked_preserving_conflict(
        &target,
        |writer| writer.write_all(b"our edit"),
        || Ok(()),
        |backup| fs::read(backup).unwrap() != b"original",
    )
    .unwrap();
    assert!(backup.is_none());
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(unix)]
#[test]
fn a_new_file_gets_the_same_mode_as_a_standard_create() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let ordinary = dir.path().join("ordinary");
    let replaced = dir.path().join("replaced");
    File::create(&ordinary).unwrap();
    write_atomic(&replaced, b"new").unwrap();
    assert_eq!(
        fs::metadata(&replaced).unwrap().permissions().mode() & 0o777,
        fs::metadata(&ordinary).unwrap().permissions().mode() & 0o777
    );
}

#[cfg(unix)]
#[test]
fn a_new_private_file_stays_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("known_hosts");
    write_atomic_private(&target, b"host key").unwrap();
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&target, fs::Permissions::from_mode(0o666)).unwrap();
    write_atomic_private(&target, b"next key").unwrap();
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(windows)]
#[test]
fn windows_replacement_keeps_hidden_flag_and_named_stream() {
    use std::os::windows::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"old").unwrap();
    let target_name = target.to_str().unwrap();
    winsafe::SetFileAttributes(target_name, winsafe::co::FILE_ATTRIBUTE::HIDDEN).unwrap();
    let stream = format!("{}:test-stream", target.display());
    fs::write(&stream, b"metadata").unwrap();
    let before = fs::metadata(&target).unwrap();
    write_atomic(&target, b"new").unwrap();
    let after = fs::metadata(&target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_ne!(after.file_attributes() & 0x2, 0, "hidden flag was lost");
    assert_eq!(fs::read(&stream).unwrap(), b"metadata");
    assert_eq!(after.created().unwrap(), before.created().unwrap());
}

#[cfg(windows)]
#[test]
fn windows_displaced_change_is_retained_after_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let ((), backup) = replace_checked_preserving_conflict(
        &target,
        |writer| writer.write_all(b"our edit"),
        || {
            // Model a writer outside this process changing the target after
            // the caller's last check.
            fs::write(&target, b"other writer")?;
            Ok(())
        },
        |backup| fs::read(backup).unwrap() != b"original",
    )
    .unwrap();
    let backup = backup.unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"our edit");
    assert_eq!(fs::read(&backup).unwrap(), b"other writer");
    fs::remove_file(backup).unwrap();
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(windows)]
#[test]
fn windows_replacement_keeps_a_protected_access_list() {
    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let user = Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    assert!(user.status.success());
    let user = String::from_utf8_lossy(&user.stdout);
    let sid = user
        .trim()
        .split(',')
        .next_back()
        .unwrap()
        .trim_matches('"');
    assert!(sid.starts_with("S-1-"), "unexpected user ID: {user}");
    let applied = Command::new("icacls")
        .arg(&target)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(F)")])
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let list = || {
        let output = Command::new("icacls").arg(&target).output().unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let before = list();
    write_atomic(&target, b"replacement").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(list(), before);
}

#[cfg(windows)]
#[test]
fn windows_replacement_retries_a_locked_temporary_source() {
    use std::cell::RefCell;
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let held = RefCell::new(None);
    let mut checks = 0;
    replace_checked(
        &target,
        |writer| {
            writer.write_all(b"replacement")?;
            let temporary = fs::read_dir(dir.path())?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(".compare-all-"))
                })
                .unwrap();
            *held.borrow_mut() = Some(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(3)
                    .open(temporary)?,
            );
            Ok(())
        },
        || -> io::Result<()> {
            checks += 1;
            if checks == 2 {
                drop(held.borrow_mut().take());
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(checks, 2, "a source sharing violation must be retried");
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(windows)]
#[test]
fn windows_failed_replace_recovery_covers_documented_error_states() {
    use winsafe::co::ERROR;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    for code in [
        ERROR::UNABLE_TO_REMOVE_REPLACED,
        ERROR::UNABLE_TO_MOVE_REPLACEMENT,
    ] {
        fs::write(&target, b"original").unwrap();
        let mut backup = tempfile::NamedTempFile::new_in(dir.path())
            .unwrap()
            .into_temp_path();
        let error = recover_windows_failure(&target, &mut backup, code);
        assert_eq!(error.raw_os_error(), Some(code.raw().cast_signed()));
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    fs::remove_file(&target).unwrap();
    let mut backup = tempfile::NamedTempFile::new_in(dir.path())
        .unwrap()
        .into_temp_path();
    fs::write(&backup, b"original").unwrap();
    let error = recover_windows_failure(&target, &mut backup, ERROR::UNABLE_TO_MOVE_REPLACEMENT_2);
    assert_eq!(error.raw_os_error(), Some(1177));
    assert_eq!(fs::read(&target).unwrap(), b"original");
    assert!(!backup.exists());

    let mut backup = tempfile::NamedTempFile::new_in(dir.path())
        .unwrap()
        .into_temp_path();
    fs::write(&backup, b"original").unwrap();
    fs::write(&target, b"another writer").unwrap();
    let retained = backup.to_path_buf();
    let error = recover_windows_failure(&target, &mut backup, ERROR::UNABLE_TO_MOVE_REPLACEMENT_2);
    assert!(error.to_string().contains(&retained.display().to_string()));
    assert_eq!(fs::read(&target).unwrap(), b"another writer");
    assert_eq!(fs::read(&retained).unwrap(), b"original");
    fs::remove_file(retained).unwrap();
}

#[cfg(windows)]
fn deny_replacement(path: &Path) -> File {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(3) // FILE_SHARE_READ | FILE_SHARE_WRITE; no FILE_SHARE_DELETE.
        .open(path)
        .unwrap()
}

#[cfg(windows)]
#[test]
fn windows_sharing_violation_keeps_original_and_cleans_up_after_retries() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let _held = deny_replacement(&target);
    assert!(write_atomic(&target, b"replacement").is_err());
    assert_eq!(fs::read(&target).unwrap(), b"original");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(windows)]
#[test]
fn windows_retry_commits_when_the_other_handle_closes() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let mut held = Some(deny_replacement(&target));
    let mut checks = 0;
    replace_checked(
        &target,
        |writer| writer.write_all(b"replacement"),
        || {
            checks += 1;
            if checks == 2 {
                drop(held.take());
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(checks, 2);
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(windows)]
#[test]
fn cancellation_is_rechecked_between_windows_rename_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("output");
    fs::write(&target, b"original").unwrap();
    let _held = deny_replacement(&target);
    let mut checks = 0;
    let result = replace_checked(
        &target,
        |writer| writer.write_all(b"replacement"),
        || {
            checks += 1;
            if checks > 1 {
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(checks, 2);
    assert_eq!(fs::read(&target).unwrap(), b"original");
    assert_eq!(names(dir.path()), ["output"]);
}

#[cfg(unix)]
#[test]
fn symlinks_are_refused_including_dangling_links() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let link = dir.path().join("link");
    fs::write(&source, b"original").unwrap();
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(write_atomic(&link, b"replacement").is_err());
    assert_eq!(fs::read(&source).unwrap(), b"original");
    fs::remove_file(&source).unwrap();
    assert!(write_atomic(&link, b"replacement").is_err());
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(names(dir.path()), ["link"]);
}

#[test]
fn a_log_opened_without_append_starts_empty_and_with_append_keeps_lines() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("run.log");
    fs::write(&target, b"old\n").unwrap();
    {
        let mut file = open_log(&target, true).unwrap();
        file.write_all(b"next\n").unwrap();
    }
    assert_eq!(fs::read(&target).unwrap(), b"old\nnext\n");
    {
        let mut file = open_log(&target, false).unwrap();
        file.write_all(b"fresh\n").unwrap();
    }
    assert_eq!(fs::read(&target).unwrap(), b"fresh\n");
    assert_eq!(names(dir.path()), ["run.log"]);
}

#[test]
fn a_log_or_a_removal_refuses_a_folder_and_a_read_only_file() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    assert!(open_log(&folder, true).is_err());
    assert!(open_log(&folder, false).is_err());
    assert!(remove_file(&folder).is_err());
    assert!(folder.is_dir());

    let locked = dir.path().join("locked.json");
    fs::write(&locked, b"{}").unwrap();
    let mut permissions = fs::metadata(&locked).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&locked, permissions.clone()).unwrap();
    assert!(remove_file(&locked).is_err());
    assert!(locked.is_file());
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&locked, permissions).unwrap();
    remove_file(&locked).unwrap();
    assert!(!locked.exists());
    assert_eq!(
        remove_file(&locked).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}
