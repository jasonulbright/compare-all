//! Numbered copies of a file, taken before an editor save replaces it.
//!
//! The names follow the rule the file operations use for a replaced file: the
//! suffixed name first, then the same name with a number appended. An existing
//! copy is never overwritten. Past the configured count the copies with the
//! lowest numbers are removed, so the newest copies stay.

use crate::save::{Baseline, FileSystem, SaveOutcome, Stamp};
use ca_fs::BackupOptions as BackupNames;
use ca_session::options::{BackupLocation, BackupOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Where the copies of a saved file go and how many stay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveBackups {
    /// The folder and the suffix the names are built from.
    pub names: BackupNames,
    /// Copies kept of one file. Zero keeps every copy.
    pub copies: u32,
}

impl SaveBackups {
    /// The rule an editor save follows, or nothing when the options take no
    /// copy before a save.
    #[must_use]
    pub fn for_save(options: &BackupOptions) -> Option<Self> {
        options.before_save.then(|| Self {
            names: names(options),
            copies: options.copies,
        })
    }
}

/// The folder and suffix the options name for a copy.
///
/// A named folder that the options leave empty writes the copy beside the
/// file, because no other folder is known.
#[must_use]
pub fn names(options: &BackupOptions) -> BackupNames {
    let folder = match options.location {
        BackupLocation::NamedFolder { .. } => options
            .folder
            .as_ref()
            .map(|folder| folder.as_path().to_path_buf())
            .filter(|folder| !folder.as_os_str().is_empty()),
        _ => None,
    };
    BackupNames {
        folder,
        suffix: options.suffix.clone(),
    }
}

/// The folder copies of `target` are written to.
fn folder_of(rule: &SaveBackups, target: &Path) -> Option<PathBuf> {
    rule.names
        .folder
        .clone()
        .or_else(|| target.parent().map(Path::to_path_buf))
}

/// The number a name carries in the sequence of `target`, if it is one.
fn index_of(rule: &SaveBackups, target: &Path, name: &str) -> Option<usize> {
    let file = target.file_name()?.to_string_lossy();
    let stem = format!("{file}{}", rule.names.suffix);
    let rest = name.strip_prefix(stem.as_str())?;
    if rest.is_empty() {
        return Some(0);
    }
    if rest.starts_with('0') || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// The numbers already taken in the sequence of `target`, lowest first.
fn taken(rule: &SaveBackups, target: &Path, folder: &Path) -> io::Result<Vec<usize>> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(index) = index_of(rule, target, &name.to_string_lossy()) {
            if rule.names.path_for_index(target, index) != target {
                found.push(index);
            }
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// Copy `target` to the next free name of its sequence and remove the copies
/// past the configured count.
///
/// A target that does not exist yet has nothing to keep, so no copy is taken.
///
/// # Errors
///
/// Returns the error of the folder read, the folder creation or the copy. A
/// copy that fails leaves the target untouched, and the caller does not save.
pub fn take(rule: &SaveBackups, target: &Path) -> io::Result<Option<PathBuf>> {
    take_with(rule, target, |source, written| {
        io::copy(source, written).map(|_| ())
    })
}

fn take_with(
    rule: &SaveBackups,
    target: &Path,
    fill: impl FnOnce(&mut std::fs::File, &mut dyn Write) -> io::Result<()>,
) -> io::Result<Option<PathBuf>> {
    if !target.is_file() {
        return Ok(None);
    }
    let Some(folder) = folder_of(rule, target) else {
        return Ok(None);
    };
    std::fs::create_dir_all(&folder)?;
    let mut numbers = taken(rule, target, &folder)?;
    let mut next = numbers.last().map_or(0, |last| last.saturating_add(1));
    // An empty suffix beside the file makes the first name the target itself.
    if rule.names.path_for_index(target, next) == target {
        next = next.saturating_add(1);
    }
    let copy = rule.names.path_for_index(target, next);
    let mut source = std::fs::File::open(target)?;
    ca_io::replace_new_checked(
        &copy,
        |written: &mut dyn Write| fill(&mut source, written),
        || Ok::<(), io::Error>(()),
    )?;
    numbers.push(next);
    // An empty suffix matches names the user owns, so nothing is removed then.
    if rule.copies > 0 && !rule.names.suffix.is_empty() {
        let keep = rule.copies as usize;
        let excess = numbers.len().saturating_sub(keep);
        for index in numbers.iter().take(excess) {
            let _ = ca_io::remove_file(&rule.names.path_for_index(target, *index));
        }
    }
    Ok(Some(copy))
}

/// A file system that takes a copy of the target before it replaces it.
#[derive(Debug, Clone)]
pub struct WithBackups<F> {
    /// The file system the save writes through.
    pub files: F,
    /// The copy rule, or nothing when no copy is taken.
    pub rule: Option<SaveBackups>,
}

impl<F: FileSystem> FileSystem for WithBackups<F> {
    fn stamp(&self, path: &Path) -> Option<Stamp> {
        self.files.stamp(path)
    }

    fn is_writable(&self, path: &Path) -> bool {
        self.files.is_writable(path)
    }

    fn replace(&self, target: &Path, bytes: &[u8]) -> io::Result<()> {
        self.files.replace(target, bytes)
    }

    fn replace_checked(
        &self,
        target: &Path,
        bytes: &[u8],
        expected: Baseline,
        accept_disk_change: bool,
    ) -> Result<(), SaveOutcome> {
        self.files
            .replace_checked(target, bytes, expected, accept_disk_change)
    }

    fn replace_checked_with_backup(
        &self,
        target: &Path,
        bytes: &[u8],
        expected: Baseline,
        accept_disk_change: bool,
    ) -> Result<Option<PathBuf>, SaveOutcome> {
        if let Some(rule) = &self.rule {
            take(rule, target).map_err(|error| {
                SaveOutcome::Failed(format!(
                    "No backup copy could be written, so the file was not saved: {error}"
                ))
            })?;
        }
        self.files
            .replace_checked_with_backup(target, bytes, expected, accept_disk_change)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{names, take, take_with, SaveBackups};
    use ca_session::options::{BackupLocation, BackupOptions};

    fn rule(copies: u32) -> SaveBackups {
        let options = BackupOptions {
            before_save: true,
            copies,
            suffix: ".bak".to_owned(),
            ..BackupOptions::default()
        };
        SaveBackups::for_save(&options).unwrap()
    }

    #[test]
    fn no_rule_exists_until_the_options_ask_for_a_copy_before_a_save() {
        assert_eq!(SaveBackups::for_save(&BackupOptions::default()), None);
    }

    #[test]
    #[cfg(unix)]
    fn pruning_keeps_links_and_read_only_copies() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        let unrelated = dir.path().join("unrelated.txt");
        let link = dir.path().join("a.txt.bak");
        let protected = dir.path().join("a.txt.bak1");
        std::fs::write(&target, "new content").unwrap();
        std::fs::write(&unrelated, "unrelated").unwrap();
        symlink(&unrelated, &link).unwrap();
        std::fs::write(&protected, "protected").unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o444)).unwrap();
        let copy = take(&rule(1), &target).unwrap().unwrap();
        assert_eq!(copy, dir.path().join("a.txt.bak2"));
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&protected).unwrap(), "protected");
        assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "unrelated");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "new content");
    }

    #[test]
    fn copies_are_numbered_and_the_oldest_go_past_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        let rule = rule(2);
        for generation in 0..4 {
            std::fs::write(&target, format!("v{generation}")).unwrap();
            take(&rule, &target).unwrap();
        }
        assert!(!dir.path().join("a.txt.bak").exists());
        assert!(!dir.path().join("a.txt.bak1").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt.bak2")).unwrap(),
            "v2"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt.bak3")).unwrap(),
            "v3"
        );
    }

    #[test]
    fn the_copy_is_invisible_under_its_final_name_until_it_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        std::fs::write(&target, "content").unwrap();
        let final_name = dir.path().join("a.txt.bak");
        let copy = take_with(&rule(1), &target, |source, written| {
            assert!(!final_name.exists(), "an incomplete copy is visible");
            std::io::copy(source, written).map(|_| ())
        })
        .unwrap()
        .unwrap();
        assert_eq!(copy, final_name);
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "content");
    }

    #[test]
    fn a_failed_copy_leaves_no_file_in_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        std::fs::write(&target, "content").unwrap();
        let error = take_with(&rule(1), &target, |_, _| {
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "disk full");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("a.txt")]);
    }

    #[test]
    fn a_missing_target_takes_no_copy() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(take(&rule(1), &dir.path().join("new.txt")).unwrap(), None);
    }

    #[test]
    fn an_empty_suffix_never_names_the_target_and_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        std::fs::write(&target, "one").unwrap();
        std::fs::write(dir.path().join("a.txt7"), "user file").unwrap();
        let rule = SaveBackups {
            names: ca_fs::BackupOptions {
                folder: None,
                suffix: String::new(),
            },
            copies: 1,
        };
        let copy = take(&rule, &target).unwrap().unwrap();
        assert_ne!(copy, target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "one");
        assert!(dir.path().join("a.txt7").exists());
    }

    #[test]
    fn a_named_folder_without_a_path_writes_beside_the_file() {
        let options = BackupOptions {
            location: BackupLocation::from_id("namedFolder"),
            ..BackupOptions::default()
        };
        assert_eq!(names(&options).folder, None);
    }
}
