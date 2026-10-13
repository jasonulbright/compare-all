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
            entries: without_inherited_marks(entries),
        }))
    }

    /// Give the file at `path` this list and nothing from its folder.
    ///
    /// # Errors
    /// Returns an error when the volume refuses the list, or when the file
    /// does not report the same list afterwards. The file can then be open
    /// to more users than the list allows.
    pub(crate) fn apply_to(&self, path: &Path) -> io::Result<()> {
        let descriptor: LocalBox<SecurityDescriptor> = format!("D:P{}", self.entries).parse()?;
        let list = descriptor
            .dacl()
            .ok_or_else(|| io::Error::other("the access list has no entries to apply"))?;
        let mut file = File::options()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .open(path)?;
        SetSecurityInfo(
            &mut file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            Some(list),
            None,
        )?;
        if Self::parse(&list_text(&file)?)?.as_ref() == Some(self) {
            Ok(())
        } else {
            Err(io::Error::other("the volume did not keep the access list"))
        }
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

/// True when the volume that holds `path` reports that it keeps no access
/// lists, as FAT and exFAT do. A volume that does not answer counts as one
/// that keeps them.
fn volume_keeps_no_lists(path: &Path) -> bool {
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
