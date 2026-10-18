//! The elevated step of an update of an install for every user.
//!
//! The helper starts the installed program, which only administrators can
//! change, through the standard elevation prompt with [`INSTALL_ARGUMENT`],
//! so the prompt names the signed program. The elevated program copies the
//! staged installer into a new folder that only administrators and the
//! system can open, checks the copy again, runs the system installer on that
//! copy and removes the folder. It writes nothing in the staging folder: that
//! folder belongs to the user.

use crate::mode::{detect, InstallMode};
use crate::signature::SignatureVerifier;
use crate::staging::{AssetKind, Staging};
use crate::verify::{hold_read_only, verify_and_hold, Expected};
use crate::version::{InstallerVersion, Version};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// First argument of the elevated program.
pub const INSTALL_ARGUMENT: &str = "--install-update";

/// The file name of the installed program.
pub const PROGRAM_NAME: &str = "compare-all.exe";

/// The `UpgradeCode` of every release installer.
pub const UPGRADE_CODE: &str = "{6F1C2A4E-3B7D-4E59-9A21-C0DE0A11C0A1}";

/// The `ProductName` of every release installer.
pub const PRODUCT_NAME: &str = "Compare All";

/// Name prefix of the folders the elevated step creates in the program files
/// folder.
pub const WORK_FOLDER_PREFIX: &str = "compare-all-update-";

/// The name the running program file gets in the work folder while the
/// installer runs.
const ASIDE_NAME: &str = "compare-all-previous.exe";

/// Exit code: the staged installer failed a check of the elevated step.
pub const REFUSED: i32 = 0x1000_0001;

/// Exit code: the elevated step could not prepare or start the install.
pub const NOT_PREPARED: i32 = 0x1000_0002;

/// What the copy of the installer must match.
pub struct Checks<'a> {
    /// The Authenticode verifier.
    pub signer: &'a dyn SignatureVerifier,
    /// Common name of the required signer.
    pub signer_name: &'a str,
    /// The build that is installed now. Only a newer build is installed.
    pub running: &'a Version,
}

/// The staging folder named by an elevated program command line (without
/// the program name).
#[must_use]
pub fn parse_install_arguments(arguments: &[OsString]) -> Option<PathBuf> {
    match arguments {
        [flag, staging] if flag == INSTALL_ARGUMENT => Some(PathBuf::from(staging)),
        _ => None,
    }
}

/// The last two folder names of every staging folder.
const STAGING_TAIL: [&str; 2] = ["compare-all", "updates"];

/// The staging folder that the elevated step reads, or `None` when it
/// refuses `staging`. The folder must be a local drive path with no `.` or
/// `..` part, end in `compare-all\updates`, and, with every link in it
/// resolved, still be such a path inside one of `profiles`. A network path,
/// a device path or a link that leads out of the profiles is refused, so the
/// elevated account never opens a remote file. The resolved path is
/// returned.
#[must_use]
pub fn accepted_staging(staging: &Path, profiles: &[PathBuf]) -> Option<PathBuf> {
    if !is_local_path(staging) {
        return None;
    }
    let resolved = resolve(staging)?;
    let tail: Vec<String> = resolved
        .components()
        .rev()
        .take(STAGING_TAIL.len())
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if !tail.iter().rev().eq(STAGING_TAIL.iter()) {
        return None;
    }
    profiles
        .iter()
        .flat_map(|profile| [Some(profile.clone()), resolve(profile)])
        .flatten()
        .any(|profile| is_local_path(&profile) && is_inside(&resolved, &profile))
        .then_some(resolved)
}

/// True for an absolute path on a drive letter with plain folder names only.
fn is_local_path(path: &Path) -> bool {
    use std::path::Component;
    let mut parts = path.components();
    #[cfg(windows)]
    {
        let Some(Component::Prefix(prefix)) = parts.next() else {
            return false;
        };
        if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
            return false;
        }
    }
    parts.next() == Some(Component::RootDir)
        && parts.all(|part| matches!(part, Component::Normal(_)))
}

/// `path` with every link resolved, as a drive path. `None` when it does not
/// exist or resolves to anything but a local drive path.
fn resolve(path: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(path).ok()?;
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut parts = resolved.components();
        let Some(Component::Prefix(prefix)) = parts.next() else {
            return None;
        };
        let Prefix::VerbatimDisk(letter) = prefix.kind() else {
            return None;
        };
        let mut plain = PathBuf::from(format!("{}:\\", char::from(letter)));
        if parts.next() != Some(Component::RootDir) {
            return None;
        }
        plain.extend(parts);
        is_local_path(&plain).then_some(plain)
    }
    #[cfg(not(windows))]
    {
        is_local_path(&resolved).then_some(resolved)
    }
}

/// True when `path` lies below `folder`; names compare without letter case.
fn is_inside(path: &Path, folder: &Path) -> bool {
    let fold = |part: std::path::Component<'_>| part.as_os_str().to_string_lossy().to_lowercase();
    let mut inner = path.components().map(fold);
    let outer: Vec<String> = folder.components().map(fold).collect();
    outer.iter().all(|part| inner.next().as_ref() == Some(part)) && inner.next().is_some()
}

/// The entry point of the elevated program. Returns the exit code: the code
/// of the installer, [`REFUSED`] or [`NOT_PREPARED`].
#[must_use]
pub fn install_main(staging: &Path, checks: &Checks<'_>) -> i32 {
    let Ok(program) = std::env::current_exe() else {
        return NOT_PREPARED;
    };
    if detect(&program) != (InstallMode::Installed { machine: true }) {
        return NOT_PREPARED;
    }
    let Some(staging) = accepted_staging(staging, &crate::system::profile_folders()) else {
        return REFUSED;
    };
    let Ok(parent) = crate::system::program_files() else {
        return NOT_PREPARED;
    };
    sweep_work_folders(&parent);
    let Ok(work) = prepare_work_folder(&parent) else {
        return NOT_PREPARED;
    };
    let code = install_from(
        &Staging::new(staging),
        &work,
        Some(&program),
        checks,
        &run_installer,
    );
    if std::fs::remove_dir_all(&work).is_err() {
        remove_after_exit(&work);
    }
    code
}

/// Copy the staged installer of `staging` into `work`, check the copy and run
/// the command from [`installer_command`] through `run`. `program`, when
/// given, is moved into `work` while the installer runs, so the installer
/// finds no program file in use, and is moved back when the install fails.
/// Returns the exit code of the installer, [`REFUSED`] or [`NOT_PREPARED`].
#[must_use]
pub fn install_from(
    staging: &Staging,
    work: &Path,
    program: Option<&Path>,
    checks: &Checks<'_>,
    run: &dyn Fn(Command) -> Option<i32>,
) -> i32 {
    let Some(record) = staging.peek_record() else {
        return REFUSED;
    };
    let Some(version) = record.parsed_version() else {
        return REFUSED;
    };
    if version <= *checks.running || record.kind() != Some(AssetKind::Msi) {
        return REFUSED;
    }
    let copy = work.join(&record.asset);
    if copy_at_most(&staging.asset_path(&record), &copy, record.size).is_err() {
        let _ = std::fs::remove_file(&copy);
        return REFUSED;
    }
    let Ok(held) = hold_read_only(&copy) else {
        let _ = std::fs::remove_file(&copy);
        return NOT_PREPARED;
    };
    let expected = Expected {
        kind: AssetKind::Msi,
        size: record.size,
        sha256: &record.sha256,
        signer: checks.signer_name,
        version: &version,
    };
    let checked = verify_and_hold(&copy, &expected, checks.signer, &work.join("scratch"))
        .and_then(|_| check_package(&held, &version, checks.running));
    if checked.is_err() {
        drop(held);
        let _ = std::fs::remove_file(&copy);
        return REFUSED;
    }
    let aside = program.and_then(|program| {
        let moved = work.join(ASIDE_NAME);
        std::fs::rename(program, &moved)
            .ok()
            .map(|()| (program, moved))
    });
    let code = run(installer_command(&copy));
    drop(held);
    let _ = std::fs::remove_file(&copy);
    if let Some((program, moved)) = aside {
        if !matches!(code, Some(0 | 1641 | 3010)) && !program.exists() {
            let _ = std::fs::rename(&moved, program);
        }
    }
    code.unwrap_or(NOT_PREPARED)
}

/// What an installer database names in its `Property` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageIdentity {
    /// `ProductName`.
    pub product_name: String,
    /// `UpgradeCode`.
    pub upgrade_code: String,
    /// `ProductVersion`.
    pub product_version: String,
}

/// Read the identity of the installer database open in `file`.
///
/// # Errors
/// Returns why the file is not a database or lacks one of the properties.
pub fn package_identity(file: &std::fs::File) -> Result<PackageIdentity, String> {
    let mut package = msi::Package::open(file)
        .map_err(|error| format!("the installer is not a valid database: {error}"))?;
    let rows = package
        .select_rows(msi::Select::table("Property").columns(&["Property", "Value"]))
        .map_err(|error| format!("the installer has no property table: {error}"))?;
    let (mut product_name, mut upgrade_code, mut product_version) = (None, None, None);
    for row in rows {
        if row.len() != 2 {
            continue;
        }
        let (Some(name), Some(value)) = (row[0].as_str(), row[1].as_str()) else {
            continue;
        };
        let slot = match name {
            "ProductName" => &mut product_name,
            "UpgradeCode" => &mut upgrade_code,
            "ProductVersion" => &mut product_version,
            _ => continue,
        };
        *slot = Some(value.to_owned());
    }
    match (product_name, upgrade_code, product_version) {
        (Some(product_name), Some(upgrade_code), Some(product_version)) => Ok(PackageIdentity {
            product_name,
            upgrade_code,
            product_version,
        }),
        _ => Err("the installer does not name its product".to_owned()),
    }
}

/// Check that the database open in `file` is a release installer of this
/// product, newer than `running`, the build the elevated program is. The
/// staged record, which the user can write, only has to agree:
/// `record_version` must have the product version of the database.
///
/// # Errors
/// Returns the first check the database fails.
pub fn check_package(
    file: &std::fs::File,
    record_version: &Version,
    running: &Version,
) -> Result<(), String> {
    let identity = package_identity(file)?;
    if !identity.upgrade_code.eq_ignore_ascii_case(UPGRADE_CODE)
        || identity.product_name != PRODUCT_NAME
    {
        return Err("the installer belongs to another product".to_owned());
    }
    let product = InstallerVersion::parse(&identity.product_version)
        .ok_or("the installer has no valid product version")?;
    let installed = running
        .installer_version()
        .ok_or("the running build has no installer version")?;
    if product <= installed {
        return Err(format!(
            "the installer holds {product}, the installed build is {installed}"
        ));
    }
    if record_version.installer_version() != Some(product) {
        return Err(format!(
            "the installer holds {product}, the staged record names {record_version}"
        ));
    }
    Ok(())
}

/// Copy `source` to `destination`, a new file, and fail unless `source` held
/// exactly `size` bytes.
fn copy_at_most(source: &Path, destination: &Path, size: u64) -> Result<(), String> {
    use std::io::Read;
    let input = crate::verify::open_without_link(source).map_err(|error| error.to_string())?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| error.to_string())?;
    let copied = std::io::copy(&mut input.take(size.saturating_add(1)), &mut output)
        .map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())?;
    if copied == size {
        Ok(())
    } else {
        Err("the staged installer has the wrong size".to_owned())
    }
}

/// The system installer command for `msi`: a major upgrade for every user
/// with a progress window, no questions and no log. The environment holds
/// only the Windows folder and drive; with no temporary folder set, the
/// installer uses the Windows folder, which only administrators can write.
#[must_use]
pub fn installer_command(msi: &Path) -> Command {
    let root = crate::system::system_root().unwrap_or_else(|_| PathBuf::from(r"C:\Windows"));
    let system = root.join("System32");
    let mut command = ca_io::host_command::host_command(system.join("msiexec.exe"));
    command
        .arg("/i")
        .arg(msi)
        .args(["/passive", "/norestart", "ALLUSERS=1"])
        .env_clear()
        .env("SystemRoot", &root)
        .current_dir(&system);
    if let Some(std::path::Component::Prefix(drive)) = root.components().next() {
        command.env("SystemDrive", drive.as_os_str());
    }
    command
}

fn run_installer(mut command: Command) -> Option<i32> {
    command.status().ok().and_then(|status| status.code())
}

/// Create the work folder of this process in `parent` and give it an access
/// list of its own: full control for administrators and the system, no
/// inherited entries, so no other account can open or change it.
///
/// # Errors
/// Returns why the folder could not be created or restricted; the folder is
/// then removed.
pub fn prepare_work_folder(parent: &Path) -> Result<PathBuf, String> {
    let work = parent.join(format!("{WORK_FOLDER_PREFIX}{}", std::process::id()));
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|error| error.to_string())?;
    }
    std::fs::create_dir(&work).map_err(|error| error.to_string())?;
    if let Err(error) = restrict_to_administrators(&work) {
        let _ = std::fs::remove_dir_all(&work);
        return Err(error);
    }
    Ok(work)
}

#[cfg(windows)]
fn restrict_to_administrators(folder: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = crate::system::system_root()?;
    let system = root.join("System32");
    let status = ca_io::host_command::host_command(system.join("icacls.exe"))
        .arg(folder)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-32-544:(OI)(CI)F",
            "*S-1-5-18:(OI)(CI)F",
            "/q",
        ])
        .env_clear()
        .env("SystemRoot", &root)
        .current_dir(&system)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|error| format!("cannot restrict the work folder: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("cannot restrict the work folder".to_owned())
    }
}

#[cfg(not(windows))]
fn restrict_to_administrators(_: &Path) -> Result<(), String> {
    Err("this platform has no installer for every user".to_owned())
}

/// Remove the work folders that earlier elevated steps left in `parent`.
/// Folders still in use stay.
pub fn sweep_work_folders(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(WORK_FOLDER_PREFIX)
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The work folder holds the running program file after a successful
/// install, which cannot be removed while this process runs. A system
/// command removes the folder once this process has ended.
#[cfg(windows)]
fn remove_after_exit(work: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(root) = crate::system::system_root() else {
        return;
    };
    let system = root.join("System32");
    let line = format!(
        "/d /s /c \"\"{}\" -n 3 127.0.0.1 >nul & rd /s /q \"{}\"\"",
        system.join("PING.EXE").display(),
        work.display()
    );
    let _ = ca_io::host_command::host_command(system.join("cmd.exe"))
        .raw_arg(line)
        .env_clear()
        .env("SystemRoot", &root)
        .current_dir(&system)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(not(windows))]
fn remove_after_exit(_: &Path) {}

#[cfg(test)]
mod tests {
    #[test]
    fn the_product_identity_is_the_identity_the_installer_source_gives() {
        let source = include_str!("../../../installer/compare-all.wxs");
        let code = super::UPGRADE_CODE.trim_matches(['{', '}']);
        assert!(source.contains(&format!("UpgradeCode=\"{code}\"")));
        assert!(source.contains(&format!("Name=\"{}\"", super::PRODUCT_NAME)));
    }
}
