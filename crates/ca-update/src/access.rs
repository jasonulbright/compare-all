//! Whether an account other than administrators and the system can change a
//! file or folder.
//!
//! An install for every user elevates the program file of its install
//! folder. When another account can change that file or folder, that account
//! chooses what the administrator runs, so no update is applied there.

use std::path::Path;

/// `S-1-5-32-544`, `S-1-5-18` and the Windows Installer trusted installer
/// service, in alias or full form.
const TRUSTED_OWNERS: [&str; 5] = [
    "BA",
    "SY",
    "S-1-5-32-544",
    "S-1-5-18",
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];

/// Creator owner and owner rights: they give rights to the owner, which
/// must be one of [`TRUSTED_OWNERS`].
const OWNER_STAND_INS: [&str; 4] = ["CO", "OW", "S-1-3-0", "S-1-3-4"];

/// Rights that let an account change, replace or delete a file, add a file
/// to a folder, or change the access list or the owner.
const CHANGE_RIGHTS: u32 = 0x0000_0002 // write data or add file
    | 0x0000_0004 // append data or add folder
    | 0x0000_0040 // delete child
    | 0x0001_0000 // delete
    | 0x0004_0000 // write access list
    | 0x0008_0000 // write owner
    | 0x1000_0000 // generic all
    | 0x4000_0000; // generic write

/// True when the security descriptor `sddl` lets an account other than
/// administrators, the system or the installer service change the object:
/// another owner, no access list, or an allow entry with a change right for
/// another account. Entries that apply only to new children count too,
/// because the unused program copy is a new child of the install folder.
#[must_use]
pub fn sddl_open_to_other_accounts(sddl: &str) -> bool {
    let Some(owner) = section(sddl, 'O') else {
        return true;
    };
    if !TRUSTED_OWNERS.contains(&owner) {
        return true;
    }
    let Some(dacl) = section(sddl, 'D') else {
        return true;
    };
    if !dacl.contains('(') {
        return true;
    }
    entries(dacl).any(|entry| {
        let fields: Vec<&str> = entry.split(';').collect();
        let [kind, _, rights, _, _, sid, ..] = fields.as_slice() else {
            return true;
        };
        matches!(*kind, "A" | "OA" | "XA" | "ZA")
            && rights_mask(rights) & CHANGE_RIGHTS != 0
            && !TRUSTED_OWNERS.contains(sid)
            && !OWNER_STAND_INS.contains(sid)
    })
}

/// The text of the `O:`, `G:`, `D:` or `S:` part of `sddl`.
fn section(sddl: &str, tag: char) -> Option<&str> {
    let mut depth = 0_u32;
    let mut start = None;
    let mut chars = sddl.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            'O' | 'G' | 'D' | 'S' if depth == 0 && chars.peek().is_some_and(|(_, c)| *c == ':') => {
                if let Some(begin) = start {
                    return Some(&sddl[begin..at]);
                }
                if ch == tag {
                    start = Some(at + 2);
                }
                chars.next();
            }
            _ => {}
        }
    }
    start.map(|begin| &sddl[begin..])
}

/// The texts inside the parentheses of an access list.
fn entries(acl: &str) -> impl Iterator<Item = &str> {
    acl.split('(')
        .skip(1)
        .map(|part| part.split(')').next().unwrap_or(""))
}

/// The access mask of an SDDL rights field. An unknown right counts as every
/// right.
fn rights_mask(rights: &str) -> u32 {
    if let Some(hex) = rights
        .strip_prefix("0x")
        .or_else(|| rights.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).unwrap_or(u32::MAX);
    }
    if !rights.len().is_multiple_of(2) {
        return u32::MAX;
    }
    let mut mask = 0_u32;
    for at in (0..rights.len()).step_by(2) {
        let Some(token) = rights.get(at..at + 2) else {
            return u32::MAX;
        };
        mask |= match token {
            "GA" => 0x1000_0000,
            "GR" => 0x8000_0000,
            "GW" => 0x4000_0000,
            "GX" => 0x2000_0000,
            "RC" => 0x0002_0000,
            "SD" => 0x0001_0000,
            "WD" => 0x0004_0000,
            "WO" => 0x0008_0000,
            "CC" => 0x0000_0001,
            "DC" => 0x0000_0002,
            "LC" => 0x0000_0004,
            "SW" => 0x0000_0008,
            "RP" => 0x0000_0010,
            "WP" => 0x0000_0020,
            "DT" => 0x0000_0040,
            "LO" => 0x0000_0080,
            "CR" => 0x0000_0100,
            "FA" => 0x001F_01FF,
            "FR" => 0x0012_0089,
            "FW" => 0x0012_0116,
            "FX" => 0x0012_00A0,
            _ => return u32::MAX,
        };
    }
    mask
}

/// True when an account other than administrators, the system or the
/// installer service can change `path`.
///
/// # Errors
/// Returns why the security descriptor could not be read.
pub fn open_to_other_accounts(path: &Path) -> Result<bool, String> {
    read_sddl(path).map(|sddl| sddl_open_to_other_accounts(&sddl))
}

/// Reads the owner and the access list through Windows `PowerShell`, which
/// gives them in the language-neutral SDDL form. The path comes from the
/// environment, never from the script text.
#[cfg(windows)]
fn read_sddl(path: &Path) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = crate::system::system_root()?;
    let shell = root.join("System32").join("WindowsPowerShell").join("v1.0");
    let output = ca_io::host_command::host_command(shell.join("powershell.exe"))
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-NoLogo",
            "-Command",
            "$ErrorActionPreference='Stop';(Get-Acl -LiteralPath $env:CA_PATH).Sddl",
        ])
        .env_clear()
        .env("SystemRoot", &root)
        .env("PSModulePath", shell.join("Modules"))
        .env("CA_PATH", path)
        .current_dir(root.join("System32"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("cannot read the access list: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && text.starts_with("O:") {
        Ok(text)
    } else {
        Err(format!("cannot read the access list of {}", path.display()))
    }
}

#[cfg(not(windows))]
fn read_sddl(_: &Path) -> Result<String, String> {
    Err("this platform has no installer for every user".to_owned())
}

#[cfg(test)]
mod tests {
    use super::sddl_open_to_other_accounts as open;

    /// A folder in the program files folder, as Windows creates it.
    const PROGRAM_FILES_CHILD: &str = "O:BAG:S-1-5-21-1-2-3-1001D:AI\
        (A;ID;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)\
        (A;CIIOID;GA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)\
        (A;ID;FA;;;SY)(A;OICIIOID;GA;;;SY)(A;ID;FA;;;BA)(A;OICIIOID;GA;;;BA)\
        (A;ID;0x1200a9;;;BU)(A;OICIIOID;GXGR;;;BU)(A;OICIIOID;GA;;;CO)\
        (A;ID;0x1200a9;;;AC)(A;OICIIOID;GXGR;;;AC)\
        (A;ID;0x1200a9;;;S-1-15-2-2)(A;OICIIOID;GXGR;;;S-1-15-2-2)";

    #[test]
    fn a_program_files_folder_is_closed_to_other_accounts() {
        assert!(!open(PROGRAM_FILES_CHILD));
        assert!(!open("O:SYD:(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;WD)"));
    }

    #[test]
    fn a_change_right_for_another_account_opens_the_object() {
        let modify = format!("{PROGRAM_FILES_CHILD}(A;OICIID;0x1301bf;;;AU)");
        assert!(open(&modify));
        assert!(open("O:BAD:(A;;FA;;;BA)(A;;FW;;;BU)"));
        assert!(open("O:BAD:(A;;FA;;;BA)(A;OICIIO;GA;;;BU)"));
        assert!(open("O:BAD:(A;;FA;;;BA)(A;;WD;;;S-1-5-21-1-2-3-1001)"));
        assert!(open("O:BAD:(A;;FA;;;BA)(A;;0X10000;;;IU)"));
        assert!(open("O:BAD:(A;;FA;;;BA)(A;;QQ;;;BU)"));
    }

    #[test]
    fn another_owner_or_no_access_list_opens_the_object() {
        assert!(open("O:S-1-5-21-1-2-3-1001G:SYD:(A;;FA;;;BA)"));
        assert!(open("O:BAG:SY"));
        assert!(open("O:BAD:NO_ACCESS_CONTROL"));
        assert!(open(""));
    }

    #[test]
    fn a_deny_entry_or_a_read_right_does_not_open_the_object() {
        assert!(!open("O:BAD:(D;;FA;;;BU)(A;;FA;;;BA)(A;;0x1200a9;;;BU)"));
    }
}
