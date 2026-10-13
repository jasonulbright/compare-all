//! The access list of a file on a Windows volume, read from one file and
//! given to another.
//!
//! A new file takes the inherited access list of its folder. A copy of a
//! file with a narrower list is therefore open to more users than the file,
//! unless the list is copied too.

use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use windows_permissions::constants::{SeObjectType, SecurityInformation};
use windows_permissions::wrappers::{
    ConvertSecurityDescriptorToStringSecurityDescriptor, GetSecurityInfo, SetSecurityInfo,
};
use windows_permissions::{LocalBox, SecurityDescriptor};

const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const DELETE: u32 = 0x0001_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
const SHARE_ALL: u32 = 0x0000_0007;

/// The text a descriptor has for a file with no access list at all, which
/// every user can open.
const NO_LIST: &str = "NO_ACCESS_CONTROL";

/// The entries of the access list of one file, in descriptor text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccessList {
    /// Every entry that applies to the file, with no mark that says which
    /// entries the file took from its folder.
    entries: String,
}

impl AccessList {
    /// The access list of the file `model`, or `None` when a copy of the file
    /// needs none: the volume keeps no access lists, or the file has none,
    /// so every user can open the file already.
    ///
    /// # Errors
    /// Returns the error of the read. No list is assumed then.
    pub(crate) fn of(model: &Path) -> io::Result<Option<Self>> {
        if volume_keeps_no_lists(model) {
            return Ok(None);
        }
        Self::parse(&list_text(&File::open(model)?)?)
    }

    /// The list that the descriptor text `text` holds.
    fn parse(text: &str) -> io::Result<Option<Self>> {
        let list = text
            .strip_prefix("D:")
            .ok_or_else(|| io::Error::other("the file reports no access list text"))?;
        let (flags, entries) = list.split_at(list.find('(').unwrap_or(list.len()));
        if names_no_list(flags)? {
            return Ok(None);
        }
        Ok(Some(Self {
            entries: without_repeats(&without_inherited_marks(entries)),
        }))
    }

    /// Create the file `path`, which no item may hold, with this list and
    /// nothing from its folder. The file is returned open and empty.
    ///
    /// A new file has the list of its folder until its own list is set, and
    /// a handle keeps the access it had when it was opened. The returned
    /// handle is the only one that can read or write the file while it is
    /// open: it shares the file for a rename or a removal only, and the list
    /// is set through it before the file holds a byte. All bytes of the file
    /// go in through this handle, so the list need not let this account
    /// write the file.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::AlreadyExists`] when an item holds `path`.
    /// Returns an error and leaves no file when the volume refuses the list,
    /// when the file does not report the same list afterwards, or when this
    /// account could not rename or remove a file with this list in the
    /// folder. When the empty file cannot be removed, the error names it.
    pub(crate) fn create(&self, path: &Path) -> io::Result<File> {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
            .share_mode(FILE_SHARE_DELETE)
            .open(path)?;
        match self.apply_to(&mut file).and_then(|()| removable(path)) {
            Ok(()) => Ok(file),
            Err(error) => Err(discard(file, path, &error)),
        }
    }

    /// Give the open file `file` this list and nothing from its folder.
    fn apply_to(&self, file: &mut File) -> io::Result<()> {
        let not_kept = |cause: &dyn std::fmt::Display| {
            io::Error::other(format!(
                "the volume of the backup folder did not keep the access list ({cause}); \
                 {NO_LISTS_ADVICE}"
            ))
        };
        let descriptor: LocalBox<SecurityDescriptor> = format!("D:P{}", self.entries).parse()?;
        let list = descriptor
            .dacl()
            .ok_or_else(|| io::Error::other("the access list has no entries to apply"))?;
        SetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            Some(list),
            None,
        )
        .map_err(|error| not_kept(&error))?;
        let kept = list_text(file).map_err(|error| not_kept(&error))?;
        if Self::parse(&kept)?.as_ref() == Some(self) {
            Ok(())
        } else {
            Err(not_kept(&"it reports another list"))
        }
    }
}

/// What a user can do when the volume of the backup folder keeps no access
/// lists.
pub(crate) const NO_LISTS_ADVICE: &str =
    "choose a backup folder on a volume that keeps access lists, or turn the backup off";

/// Fails when this account may not rename or remove the file at `path`,
/// which another handle holds open and shares for that.
///
/// The right comes from the list of the file or from the list of its
/// folder. A file without it cannot take its final name and stays under its
/// temporary name.
fn removable(path: &Path) -> io::Result<()> {
    match File::options()
        .access_mode(DELETE)
        .share_mode(SHARE_ALL)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the access list gives this account no right to rename or delete the backup, and \
             the backup folder gives no right to delete its files; choose a backup folder where \
             this account can delete files, or turn the backup off",
        )),
        Err(error) => Err(error),
    }
}

/// Remove the empty file `file` at `path` after `error`, and return the
/// error to report.
///
/// The list of the file can deny its removal, so the file takes the list of
/// its folder first. It holds no byte.
fn discard(mut file: File, path: &Path, error: &io::Error) -> io::Error {
    if let Ok(inherited) = "D:".parse::<LocalBox<SecurityDescriptor>>() {
        let _ = SetSecurityInfo(
            &mut file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::UnprotectedDacl,
            None,
            None,
            inherited.dacl(),
            None,
        );
    }
    drop(file);
    // The kind for a taken name asks the caller for another name.
    let kind = match error.kind() {
        io::ErrorKind::AlreadyExists => io::ErrorKind::Other,
        kind => kind,
    };
    match std::fs::remove_file(path) {
        Ok(()) => io::Error::new(kind, error.to_string()),
        Err(removal) => io::Error::new(
            kind,
            format!(
                "{error}; the empty temporary file {} could not be removed: {removal}",
                path.display()
            ),
        ),
    }
}

/// True when `flags`, the text between `D:` and the first entry of a list,
/// holds the mark of a file with no list.
///
/// The flags follow one another with no separator, and the mark stands
/// after the other flags of the file (`PAINO_ACCESS_CONTROL`), so its place
/// in the text is not fixed.
///
/// # Errors
/// Returns an error for a flag that is not known. No list is assumed then.
fn names_no_list(mut flags: &str) -> io::Result<bool> {
    let mut no_list = false;
    while !flags.is_empty() {
        let flag = [NO_LIST, "AR", "AI", "P"]
            .into_iter()
            .find(|flag| flags.starts_with(flag))
            .ok_or_else(|| {
                io::Error::other("the file reports an access list flag with no known meaning")
            })?;
        no_list |= flag == NO_LIST;
        flags = &flags[flag.len()..];
    }
    Ok(no_list)
}

/// The access list of the open file `file` as descriptor text.
fn list_text(file: &File) -> io::Result<String> {
    let descriptor = GetSecurityInfo(
        file,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl,
    )?;
    ConvertSecurityDescriptorToStringSecurityDescriptor(&descriptor, SecurityInformation::Dacl)?
        .into_string()
        .map_err(|_| io::Error::other("the access list text is not Unicode"))
}

/// `entries` with the mark `ID` taken out of the flags of every entry. The
/// mark says that the file took the entry from its folder; the entry grants
/// the same access with or without it.
///
/// An entry is `(type;flags;rights;;;account)`. A conditional entry holds
/// more parentheses after these fields, so only the first two fields of an
/// entry at the outer level are read.
fn without_inherited_marks(entries: &str) -> String {
    let mut out = String::with_capacity(entries.len());
    let mut depth = 0usize;
    let mut field = 0usize;
    let mut flags = String::new();
    for c in entries.chars() {
        match c {
            '(' => {
                depth += 1;
                if depth == 1 {
                    field = 0;
                }
            }
            ')' => depth = depth.saturating_sub(1),
            ';' if depth == 1 => {
                if field == 1 {
                    out.push_str(&flags.replace("ID", ""));
                    flags.clear();
                }
                field += 1;
            }
            _ if depth == 1 && field == 1 => {
                flags.push(c);
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    out
}

/// `entries` without each entry that repeats an earlier entry exactly.
///
/// An entry of a file can equal an entry the file took from its folder.
/// Windows stores one of two equal entries of some kinds, so the list it
/// reports after a write can be shorter than the list written. An entry that
/// repeats an earlier one changes no access decision.
fn without_repeats(entries: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = String::with_capacity(entries.len());
    let mut depth = 0usize;
    let mut start = 0usize;
    for (at, c) in entries.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    start = at;
                }
                depth += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && seen.insert(&entries[start..=at]) {
                    out.push_str(&entries[start..=at]);
                }
            }
            _ => {}
        }
    }
    out
}

/// True when the volume that holds `path` reports that it keeps no access
/// lists, as FAT and exFAT do. A volume that does not answer counts as one
/// that keeps them.
pub(crate) fn volume_keeps_no_lists(path: &Path) -> bool {
    volume_flags(path).is_some_and(|flags| !flags.has(winsafe::co::FILE_VOL::PERSISTENT_ACLS))
}

/// True when the volume that holds `path` reports that it keeps no access
/// lists and no named streams, so a rename over a file there drops nothing
/// that a replacement keeps. A volume that does not answer counts as one
/// that keeps them.
pub(crate) fn volume_keeps_nothing_of_a_replaced_file(path: &Path) -> bool {
    volume_flags(path).is_some_and(|flags| {
        !flags.has(winsafe::co::FILE_VOL::PERSISTENT_ACLS)
            && !flags.has(winsafe::co::FILE_VOL::NAMED_STREAMS)
    })
}

/// What the volume that holds `path` reports about itself, or `None` when it
/// does not answer.
fn volume_flags(path: &Path) -> Option<winsafe::co::FILE_VOL> {
    let absolute = std::path::absolute(path).ok()?;
    let root = winsafe::GetVolumePathName(&absolute.to_string_lossy()).ok()?;
    let mut flags = winsafe::co::FILE_VOL::default();
    winsafe::GetVolumeInformation(Some(&root), None, None, None, Some(&mut flags), None).ok()?;
    Some(flags)
}

/// The access list of the file or folder `path` as descriptor text.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) fn text_for_test(path: &Path) -> String {
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let item = File::options()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap();
    list_text(&item).unwrap()
}

/// Give the file or folder `path` the list of the descriptor text `text`,
/// or no list at all for `None`. A text that starts with `D:P` adds nothing
/// from the folder of `path`.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) fn set_for_test(path: &Path, text: Option<&str>) {
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let mut item = File::options()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap();
    let descriptor: Option<LocalBox<SecurityDescriptor>> = text.map(|text| text.parse().unwrap());
    let inheritance = if text.is_some_and(|text| text.starts_with("D:P")) {
        SecurityInformation::ProtectedDacl
    } else {
        SecurityInformation::UnprotectedDacl
    };
    SetSecurityInfo(
        &mut item,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | inheritance,
        None,
        None,
        descriptor.as_ref().and_then(|descriptor| descriptor.dacl()),
        None,
    )
    .unwrap();
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{set_for_test, text_for_test, without_inherited_marks, AccessList};

    #[test]
    fn the_inherited_mark_is_the_only_text_taken_out_of_an_entry() {
        assert_eq!(
            without_inherited_marks(
                "(A;ID;FA;;;SY)(A;OICIID;0x1301bf;;;AU)(D;;FW;;;S-1-5-21-1-2-3-1001)"
            ),
            "(A;;FA;;;SY)(A;OICI;0x1301bf;;;AU)(D;;FW;;;S-1-5-21-1-2-3-1001)"
        );
        // The account `ID` and the text of a condition are no flags.
        let conditional = r#"(XA;ID;FX;;;WD;(@User.Title=="ID;(x)"))(A;;FA;;;ID)"#;
        assert_eq!(
            without_inherited_marks(conditional),
            r#"(XA;;FX;;;WD;(@User.Title=="ID;(x)"))(A;;FA;;;ID)"#
        );
    }

    #[test]
    fn a_file_with_no_list_needs_no_copy_of_one_and_an_empty_list_is_kept() {
        for no_list in [
            "D:NO_ACCESS_CONTROL",
            "D:PAINO_ACCESS_CONTROL",
            "D:AINO_ACCESS_CONTROL",
            "D:PARAINO_ACCESS_CONTROL",
            "D:NO_ACCESS_CONTROLPAI",
        ] {
            assert_eq!(AccessList::parse(no_list).unwrap(), None, "{no_list}");
        }
        assert!(AccessList::parse("D:PX(A;;FA;;;SY)").is_err());
        let flagged = AccessList::parse("D:PARAI(A;;FA;;;SY)").unwrap().unwrap();
        assert_eq!(flagged.entries, "(A;;FA;;;SY)");
        let empty = AccessList::parse("D:P").unwrap().unwrap();
        assert_eq!(empty.entries, "");
        let inherited = AccessList::parse("D:AI(A;ID;FA;;;SY)").unwrap().unwrap();
        let protected = AccessList::parse("D:PAI(A;;FA;;;SY)").unwrap().unwrap();
        assert_eq!(inherited, protected);
        assert!(AccessList::parse("O:SY").is_err());
    }

    #[test]
    fn an_entry_that_repeats_an_earlier_entry_is_read_once() {
        // The file holds an entry that its folder gives too.
        let both = "D:AI(A;;FA;;;SY)(A;ID;0x1200a9;;;BU)(A;ID;FA;;;SY)(A;ID;FA;;;BA)";
        let read = AccessList::parse(both).unwrap().unwrap();
        assert_eq!(read.entries, "(A;;FA;;;SY)(A;;0x1200a9;;;BU)(A;;FA;;;BA)");
        let stored = AccessList::parse("D:PAI(A;;FA;;;SY)(A;;0x1200a9;;;BU)(A;;FA;;;BA)");
        assert_eq!(stored.unwrap().unwrap(), read);

        // Entries that differ in one field, and the order, stay.
        let near = "(A;;FA;;;SY)(D;;FA;;;SY)(A;;FR;;;SY)(A;OI;FA;;;SY)(A;;FA;;;BA)";
        assert_eq!(super::without_repeats(near), near);
        let conditional =
            r#"(XA;;FX;;;WD;(@User.Title=="(x)"))(A;;FA;;;SY)(XA;;FX;;;WD;(@User.Title=="(x)"))"#;
        assert_eq!(
            super::without_repeats(conditional),
            r#"(XA;;FX;;;WD;(@User.Title=="(x)"))(A;;FA;;;SY)"#
        );
    }

    #[test]
    fn a_real_file_with_no_list_at_all_reports_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("open to all.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(AccessList::of(&file).unwrap().is_some());
        set_for_test(&file, None);
        let text = text_for_test(&file);
        assert!(text.ends_with("NO_ACCESS_CONTROL"), "{text}");
        assert_eq!(AccessList::of(&file).unwrap(), None);
    }
}
