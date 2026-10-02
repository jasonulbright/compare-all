//! Regression tests for saves against real files and competing writers.
#![allow(clippy::unwrap_used, missing_docs)]

use ca_ui::save::{check_disk_change, save, Baseline, FileSystem, RealFileSystem, SaveOutcome};

#[cfg(windows)]
#[test]
fn checked_windows_save_removes_its_backup_when_nothing_changed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("notes.txt");
    std::fs::write(&target, b"original").unwrap();
    let baseline = Baseline::of(&RealFileSystem, &target);
    assert!(matches!(
        save(&RealFileSystem, &target, b"our edit", baseline, false),
        SaveOutcome::Saved(_)
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"our edit");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn displaced_windows_change_differs_from_the_save_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("notes.txt");
    std::fs::write(&target, b"original").unwrap();
    let baseline = RealFileSystem.stamp(&target).unwrap();
    let ((), backup) = ca_io::replace_checked_preserving_conflict(
        &target,
        |writer| writer.write_all(b"our edit"),
        || {
            std::fs::write(&target, b"other writer")?;
            Ok(())
        },
        |backup| RealFileSystem.stamp(backup) != Some(baseline),
    )
    .unwrap();
    let backup = backup.unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"our edit");
    assert_eq!(std::fs::read(&backup).unwrap(), b"other writer");
    std::fs::remove_file(backup).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn a_replaced_file_is_detected_even_when_size_and_time_match() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("notes.txt");
    std::fs::write(&target, b"before").unwrap();
    let before = RealFileSystem.stamp(&target).unwrap();
    ca_io::write_atomic(&target, b"after!").unwrap();
    let after = RealFileSystem.stamp(&target).unwrap();
    assert_ne!(before.identity, after.identity);
    let same_size_and_time = ca_ui::save::Stamp {
        size: after.size,
        modified: after.modified,
        identity: before.identity,
    };
    assert_eq!(
        check_disk_change(
            &RealFileSystem,
            &target,
            Baseline::Present(same_size_and_time),
            false,
        ),
        Some(SaveOutcome::ChangedOnDisk)
    );
}

#[cfg(windows)]
#[test]
fn a_windows_sharing_violation_leaves_the_saved_file_intact() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("notes.txt");
    std::fs::write(&target, b"original").unwrap();
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&target)
        .unwrap();
    assert!(matches!(
        save(
            &RealFileSystem,
            &target,
            b"edited",
            Baseline::Unchecked,
            false
        ),
        SaveOutcome::Failed(_)
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn saving_preserves_an_existing_sibling_with_the_old_temporary_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("notes.txt");
    let sibling = dir.path().join(".notes.txt.saving");
    std::fs::write(&target, b"original").unwrap();
    std::fs::write(&sibling, b"unrelated user data").unwrap();
    assert!(matches!(
        save(
            &RealFileSystem,
            &target,
            b"edited",
            Baseline::Unchecked,
            false
        ),
        SaveOutcome::Saved(_)
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"edited");
    assert_eq!(std::fs::read(&sibling).unwrap(), b"unrelated user data");
}

#[test]
fn overlapping_saves_own_different_temporary_files() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("notes.txt");
    ca_io::replace(&target, |writer| -> std::io::Result<()> {
        writer.write_all(b"first")?;
        RealFileSystem.replace(&target, b"second")?;
        assert_eq!(std::fs::read(&target)?, b"second");
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"first");
}

#[cfg(unix)]
#[test]
fn saving_preserves_private_and_executable_permission_bits() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("script");
    for mode in [0o600, 0o700, 0o755] {
        std::fs::write(&target, b"original").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(matches!(
            save(
                &RealFileSystem,
                &target,
                b"edited",
                Baseline::Unchecked,
                false
            ),
            SaveOutcome::Saved(_)
        ));
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
}
