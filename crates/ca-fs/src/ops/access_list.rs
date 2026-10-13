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
        if list.starts_with(NO_LIST) {
            return Ok(None);
        }
        let entries = list.find('(').map_or("", |start| &list[start..]);
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
    let Ok(absolute) = std::path::absolute(path) else {
        return false;
    };
    let Ok(root) = winsafe::GetVolumePathName(&absolute.to_string_lossy()) else {
        return false;
    };
    let mut flags = winsafe::co::FILE_VOL::default();
    winsafe::GetVolumeInformation(Some(&root), None, None, None, Some(&mut flags), None).is_ok()
        && !flags.has(winsafe::co::FILE_VOL::PERSISTENT_ACLS)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{without_inherited_marks, AccessList};

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
        assert_eq!(AccessList::parse("D:NO_ACCESS_CONTROL").unwrap(), None);
        let empty = AccessList::parse("D:P").unwrap().unwrap();
        assert_eq!(empty.entries, "");
        let inherited = AccessList::parse("D:AI(A;ID;FA;;;SY)").unwrap().unwrap();
        let protected = AccessList::parse("D:PAI(A;;FA;;;SY)").unwrap().unwrap();
        assert_eq!(inherited, protected);
        assert!(AccessList::parse("O:SY").is_err());
    }
}
