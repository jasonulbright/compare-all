//! The elevated step of an update of an install for every user.
//!
//! The helper starts the installed program, which only administrators can
//! change, through the standard elevation prompt with [`INSTALL_ARGUMENT`],
//! so the prompt names the signed program. The elevated program copies the
//! staged installer into a folder that only administrators and the system
//! can open, checks the copy again and runs the system installer on that
//! copy. Windows Installer records the folder of the copy as the source for
//! a later repair, so after a successful install the copy stays in a folder
//! of its own for that version; the copies of other versions are removed.
//! It writes nothing in the staging folder: that folder belongs to the user.

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

/// Name of the folder in the program files folder that holds one folder for
/// each installed version with its installer copy, named by the product
/// version of the package. The name must not start
/// with [`WORK_FOLDER_PREFIX`]: a sweep removes those folders.
pub const PACKAGE_FOLDER: &str = "compare-all-package";

/// The name the running program file gets in the work folder while the
/// installer runs.
const ASIDE_NAME: &str = "compare-all-previous.exe";

/// The name the unused copy of the program file has in the install folder
/// until it takes the place of the running file.
const STANDBY_NAME: &str = "compare-all-standby.exe";

/// Exit code: the staged installer failed a check of the elevated step.
pub const REFUSED: i32 = 0x1000_0001;

/// Exit code: the elevated step could not prepare or start the install.
pub const NOT_PREPARED: i32 = 0x1000_0002;

/// Exit code: the elevated step changed nothing and did not start the
/// installer; the staged update stays for a later close.
pub const KEPT: i32 = 0x1000_0003;

/// Exit code: this install cannot apply an update by itself, because the
/// install folder or program file is open to other accounts, the install
/// folder is on another volume than the program files folder, or the staging
/// folder is not one the elevated step reads. A later close gives the same
/// result.
pub const NOT_APPLICABLE: i32 = 0x1000_0004;

/// True when the install folder of `program` and `program` itself are closed
/// to accounts other than administrators, the system and the installer
/// service, and no such account can move the install folder away: every
/// folder above it, up to the drive root, refuses them deletion, deletion of
/// a child and changes of its access list or owner. A path with a link in
/// it, or a descriptor that cannot be read, counts as open.
#[must_use]
pub fn install_folder_closed(program: &Path) -> bool {
    let Some(resolved) = resolve(program) else {
        return false;
    };
    let fold = |path: &Path| -> Vec<String> {
        path.components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    if fold(&resolved) != fold(program) {
        return false;
    }
    let Some(folder) = resolved.parent() else {
        return false;
    };
    let mut paths = vec![resolved.as_path(), folder];
    paths.extend(folder.ancestors().skip(1));
    let Ok(descriptors) = crate::access::read_sddls(&paths) else {
        return false;
    };
    let (own, above) = descriptors.split_at(descriptors.len().min(2));
    own.iter()
        .all(|sddl| !crate::access::sddl_open_to_other_accounts(sddl))
        && above
            .iter()
            .all(|sddl| !crate::access::sddl_open_as_ancestor(sddl))
}

/// True when the elevated step can set `program` aside in a work folder in
/// `work_parent`: the install folder is on the volume of `work_parent`,
/// which a second name of the program file needs, and it is closed to other
/// accounts. A volume without access lists, such as FAT, gives every object
/// the owner Everyone and no access list, so it fails the closed check.
#[must_use]
pub fn install_folder_usable(program: &Path, work_parent: &Path) -> bool {
    program
        .parent()
        .is_some_and(|folder| same_volume(folder, work_parent))
        && install_folder_closed(program)
}

/// True when `first` and `second` exist and, with every link resolved, are
/// on the same volume: the same volume root, serial number and file system.
/// A path that cannot be resolved counts as another volume.
#[must_use]
pub fn same_volume(first: &Path, second: &Path) -> bool {
    matches!(
        (volume_identity(first), volume_identity(second)),
        (Some(a), Some(b)) if a == b
    )
}

/// The volume that holds `path`: the folder the volume is mounted at, its
/// serial number and its file system. A folder that mounts another volume
/// keeps its drive letter when links are resolved, so the drive alone does
/// not name the volume.
#[cfg(windows)]
fn volume_identity(path: &Path) -> Option<(String, u32, String)> {
    let resolved = resolve(path)?;
    let root = winsafe::GetVolumePathName(resolved.to_str()?).ok()?;
    let (mut serial, mut file_system) = (0, String::new());
    winsafe::GetVolumeInformation(
        Some(&root),
        None,
        Some(&mut serial),
        None,
        None,
        Some(&mut file_system),
    )
    .ok()?;
    Some((root.to_lowercase(), serial, file_system))
}

#[cfg(not(windows))]
fn volume_identity(path: &Path) -> Option<(String, u32, String)> {
    use std::os::unix::fs::MetadataExt;
    let resolved = resolve(path)?;
    let device = std::fs::metadata(&resolved).ok()?.dev();
    Some((device.to_string(), 0, String::new()))
}

/// True when an update of the install for every user at `program` can be
/// applied from `staging`: the elevated step would read that staging folder,
/// and the install folder is usable in the sense of
/// [`install_folder_usable`] for a work folder in the program files folder.
/// The helper asks this before the elevation prompt, so a prompt that can
/// only end in a refusal is not shown.
#[must_use]
pub fn install_place_accepted(staging: &Path, program: &Path) -> bool {
    accepted_staging(staging, &crate::system::profile_folders()).is_some()
        && crate::system::program_files()
            .is_ok_and(|parent| install_folder_usable(program, &parent))
}

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
/// of the installer, [`REFUSED`], [`NOT_PREPARED`], [`KEPT`] or
/// [`NOT_APPLICABLE`].
#[must_use]
pub fn install_main(staging: &Path, checks: &Checks<'_>) -> i32 {
    let Ok(program) = std::env::current_exe() else {
        return NOT_PREPARED;
    };
    if detect(&program) != (InstallMode::Installed { machine: true }) {
        return NOT_PREPARED;
    }
    let Ok(parent) = crate::system::program_files() else {
        return NOT_PREPARED;
    };
    // Every elevated step uses the same program name and unused-copy name,
    // so only one may set aside, install and restore at a time.
    let Some(_install_lock) = hold_install_lock(&parent, INSTALL_LOCK_WAIT) else {
        return KEPT;
    };
    let _ = remove_stale_standby(&program);
    sweep_work_folders(&parent);
    if !install_folder_usable(&program, &parent) {
        return NOT_APPLICABLE;
    }
    let Some(staging) = accepted_staging(staging, &crate::system::profile_folders()) else {
        return NOT_APPLICABLE;
    };
    let staging = Staging::new(staging);
    // The elevated program is the installed program file, so its version is
    // the installed version: the only package folder that must stay.
    let packages = parent.join(PACKAGE_FOLDER);
    if let Some(installed) = checks.running.installer_version() {
        remove_other_package_folders(&packages, &installed);
    }
    let Ok(work) = prepare_work_folder(&parent) else {
        return KEPT;
    };
    let code = install_from(
        &staging,
        work.path(),
        &packages,
        Some(&program),
        checks,
        &run_installer,
    );
    let path = work.path().to_path_buf();
    drop(work);
    if std::fs::remove_dir_all(&path).is_err() {
        remove_after_exit(&path);
    }
    code
}

/// Create the folder for the installer copy of `version` below `packages`,
/// open to administrators and the system only, and `packages` itself. A
/// folder of the same version that an earlier step left is removed first, so
/// `version` must be newer than the installed version. The installer
/// registers this folder as the source of the product, so a repair reads the
/// copy from it.
///
/// # Errors
/// Returns why a folder could not be created or restricted. A folder that
/// could not be restricted is removed.
pub fn prepare_package_folder(
    packages: &Path,
    version: &InstallerVersion,
) -> Result<PathBuf, String> {
    restricted_folder(packages)?;
    let folder = packages.join(version.to_string());
    if folder.exists() {
        std::fs::remove_dir_all(&folder)
            .map_err(|error| format!("cannot remove {}: {error}", folder.display()))?;
    }
    restricted_folder(&folder).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&folder);
    })?;
    Ok(folder)
}

fn restricted_folder(path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("cannot create {}: {error}", path.display())),
    }
    restrict_to_administrators(path)
}

/// Remove every entry of `packages` except the folder of `keep`, and
/// `packages` itself when nothing is left. An entry that cannot be removed is
/// tried again by the next step.
pub fn remove_other_package_folders(packages: &Path, keep: &InstallerVersion) {
    let keep = keep.to_string();
    let Ok(entries) = std::fs::read_dir(packages) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == keep {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    // Fails while an entry remains.
    let _ = std::fs::remove_dir(packages);
}

/// Remove the folder of `version` below `packages`, and `packages` itself
/// when nothing is left.
pub fn remove_package_folder(packages: &Path, version: &InstallerVersion) {
    let _ = std::fs::remove_dir_all(packages.join(version.to_string()));
    // Fails while a folder remains.
    let _ = std::fs::remove_dir(packages);
}

/// The staged installer of one record, as the checks of the copies see it.
struct Candidate<'a> {
    record: &'a crate::staging::StagedRecord,
    version: &'a Version,
    checks: &'a Checks<'a>,
    scratch: PathBuf,
}

impl Candidate<'_> {
    /// Copy `source` to the new file `copy`, hold the copy and check it.
    /// Returns the hold and the product version of the checked copy. A copy
    /// that fails a check is removed.
    fn copy_and_check(
        &self,
        source: &Path,
        copy: &Path,
    ) -> Result<(std::fs::File, InstallerVersion), i32> {
        let record = self.record;
        if copy_at_most(source, copy, record.size).is_err() {
            let _ = std::fs::remove_file(copy);
            return Err(REFUSED);
        }
        let Ok(held) = hold_read_only(copy) else {
            let _ = std::fs::remove_file(copy);
            return Err(NOT_PREPARED);
        };
        let expected = Expected {
            kind: AssetKind::Msi,
            size: record.size,
            sha256: &record.sha256,
            signer: self.checks.signer_name,
            version: self.version,
        };
        let checked = verify_and_hold(copy, &expected, self.checks.signer, &self.scratch)
            .and_then(|_| check_package(&held, self.version, self.checks.running));
        if let Ok(product) = checked {
            return Ok((held, product));
        }
        drop(held);
        let _ = std::fs::remove_file(copy);
        Err(REFUSED)
    }
}

/// Copy the staged installer of `staging` twice: into `work` to learn the
/// product version of the checked package, then into the folder of that
/// version below `packages`, where the copy is checked again. Run the command
/// from [`installer_command`] on the second copy through `run`.
///
/// The staged record is read once. The name of the package folder is the
/// product version of the checked copy, never a value of the record, and no
/// folder is created or removed unless that version is newer than
/// `checks.running`. After the installer succeeds, the copy stays and every
/// other folder below `packages` is removed; otherwise only the new folder is
/// removed, so the source of the installed product stays valid. `work` holds
/// the scratch files. `program`, when given, is the running program file: it
/// keeps only a name in `work` while the installer runs and an unused copy
/// takes its place, so the installer finds no program file in use, and it
/// goes back when the install fails. Returns the exit code of the installer,
/// [`REFUSED`], [`NOT_PREPARED`], or [`KEPT`] or [`NOT_APPLICABLE`] when the
/// program file or a folder cannot be prepared.
#[must_use]
pub fn install_from(
    staging: &Staging,
    work: &Path,
    packages: &Path,
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
    let candidate = Candidate {
        record: &record,
        version: &version,
        checks,
        scratch: work.join("scratch"),
    };
    let source = staging.asset_path(&record);
    let trial = work.join(&record.asset);
    let product = match candidate.copy_and_check(&source, &trial) {
        Ok((held, product)) => {
            drop(held);
            let _ = std::fs::remove_file(&trial);
            product
        }
        Err(code) => return code,
    };
    let Ok(folder) = prepare_package_folder(packages, &product) else {
        // Fails while a folder remains.
        let _ = std::fs::remove_dir(packages);
        return KEPT;
    };
    let copy = folder.join(&record.asset);
    let held = match candidate.copy_and_check(&source, &copy) {
        Ok((held, again)) if again == product => held,
        Ok(_) => {
            remove_package_folder(packages, &product);
            return REFUSED;
        }
        Err(code) => {
            remove_package_folder(packages, &product);
            return code;
        }
    };
    let aside = match program {
        Some(program) => match set_aside(program, work) {
            Ok(moved) => Some((program, moved)),
            Err(code) => {
                drop(held);
                remove_package_folder(packages, &product);
                return code;
            }
        },
        None => None,
    };
    let code = run(installer_command(&copy));
    drop(held);
    if matches!(code, Some(0 | 1641 | 3010)) {
        remove_other_package_folders(packages, &product);
    } else {
        remove_package_folder(packages, &product);
        if let Some((program, moved)) = aside {
            // Replaces the unused copy; a running file may be renamed.
            let _ = rename_retrying(&moved, program, RESTORE_ATTEMPTS);
        }
    }
    code.unwrap_or(NOT_PREPARED)
}

/// Give the running `program` a second name in `work` and put an unused
/// copy of it under the program name. Returns the name in `work`, which is
/// then the only name of the running file. When the program file is as it
/// was, returns [`NOT_APPLICABLE`] when no second name can ever be made in
/// `work`, otherwise [`KEPT`].
///
/// The program name is replaced by one rename and never removed, so an
/// interruption at any step leaves a complete program file under it.
fn set_aside(program: &Path, work: &Path) -> Result<PathBuf, i32> {
    set_aside_observed(program, work, &|| {})
}

/// [`set_aside`] that calls `after_step` after each change on the disk.
///
/// # Errors
/// As [`set_aside`].
pub fn set_aside_observed(
    program: &Path,
    work: &Path,
    after_step: &dyn Fn(),
) -> Result<PathBuf, i32> {
    let standby = program.parent().ok_or(KEPT)?.join(STANDBY_NAME);
    if !remove_standby(&standby) {
        return Err(KEPT);
    }
    let moved = work.join(ASIDE_NAME);
    // Windows refuses to replace the last name of a running program file;
    // with a second name the replace only removes a link.
    if let Err(error) = std::fs::hard_link(program, &moved) {
        return Err(link_failure_code(&error));
    }
    after_step();
    // A token may make its account the owner of new objects, and an owner
    // can change the access list from an unelevated process. The copy is
    // written in the work folder, where an owner may only read, and moves to
    // the install folder only once administrators own it; it then takes the
    // access list of the install folder.
    let fresh = work.join(STANDBY_NAME);
    let prepared = copy_flushed(program, &fresh).is_ok()
        && own_by_administrators(&fresh).is_ok()
        && rename_retrying(&fresh, &standby, SET_ASIDE_ATTEMPTS).is_ok()
        && inherit_access_list(&standby).is_ok();
    if !prepared {
        let _ = std::fs::remove_file(&fresh);
        let _ = std::fs::remove_file(&standby);
        let _ = std::fs::remove_file(&moved);
        return Err(KEPT);
    }
    after_step();
    if rename_retrying(&standby, program, SET_ASIDE_ATTEMPTS).is_err() {
        let _ = std::fs::remove_file(&standby);
        let _ = std::fs::remove_file(&moved);
        return Err(KEPT);
    }
    after_step();
    Ok(moved)
}

/// The exit code for a second name of the program file that could not be
/// made: [`NOT_APPLICABLE`] when the work folder is on another volume, a
/// result every later close repeats, otherwise [`KEPT`].
#[must_use]
pub fn link_failure_code(error: &std::io::Error) -> i32 {
    const ERROR_NOT_SAME_DEVICE: i32 = 17;
    if error.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE) {
        NOT_APPLICABLE
    } else {
        KEPT
    }
}

/// Attempts, 100 ms apart, to move the unused program copy into place.
const SET_ASIDE_ATTEMPTS: u32 = 30;

/// Attempts, 100 ms apart, to put the running program file back. A failed
/// install leaves an unused copy under the program name until this succeeds,
/// and a loaded machine can keep a scanner on that copy for longer than the
/// set-aside waits.
const RESTORE_ATTEMPTS: u32 = 600;

/// Rename `from` over `to`. A file scanner opens a new program file for a
/// short time without delete sharing; that refusal is waited out for at most
/// `attempts` tries.
fn rename_retrying(from: &Path, to: &Path, attempts: u32) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < attempts => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Remove the unused program copy that an interrupted step left in the
/// install folder of `program`. False when one is there and stays.
#[must_use]
pub fn remove_stale_standby(program: &Path) -> bool {
    program
        .parent()
        .is_some_and(|folder| remove_standby(&folder.join(STANDBY_NAME)))
}

fn remove_standby(standby: &Path) -> bool {
    match std::fs::remove_file(standby) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Copy `source` to `destination`, a new file, and flush it to the disk.
fn copy_flushed(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()
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
/// `record_version` must have the product version of the database. Returns
/// that product version.
///
/// # Errors
/// Returns the first check the database fails.
pub fn check_package(
    file: &std::fs::File,
    record_version: &Version,
    running: &Version,
) -> Result<InstallerVersion, String> {
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
    Ok(product)
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

/// The file in the program files folder that one elevated step holds from
/// before the set-aside until after the restore.
pub const INSTALL_LOCK_NAME: &str = "compare-all-update.lock";

/// How long an elevated step waits for another one to release the install
/// lock before it keeps the staged update for a later close.
const INSTALL_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The install lock. The file shares nothing and is deleted when its handle
/// closes, also when the process ends in any other way.
#[derive(Debug)]
pub struct InstallLock {
    _file: std::fs::File,
}

/// Hold the install lock in `parent`, waiting at most `wait` while another
/// step holds it. `None` when it stays held or cannot be created.
#[must_use]
pub fn hold_install_lock(parent: &Path, wait: std::time::Duration) -> Option<InstallLock> {
    let started = std::time::Instant::now();
    loop {
        if let Ok(file) = open_install_lock(&parent.join(INSTALL_LOCK_NAME)) {
            return Some(InstallLock { _file: file });
        }
        if started.elapsed() >= wait {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[cfg(windows)]
fn open_install_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const DELETE: u32 = 0x0001_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .access_mode(GENERIC_WRITE | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
}

#[cfg(not(windows))]
fn open_install_lock(_: &Path) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::other(
        "this platform has no installer for every user",
    ))
}

/// The file each elevated step keeps open in its work folder while it runs.
const LOCK_NAME: &str = "running.lock";

/// The work folder of one elevated step. While this value lives, the lock
/// file in the folder stays open without delete sharing, so a sweep by
/// another elevated step cannot remove it and leaves the folder alone.
#[derive(Debug)]
pub struct WorkFolder {
    path: PathBuf,
    _lock: std::fs::File,
}

impl WorkFolder {
    /// The folder.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Create a work folder of its own in `parent`, hold its lock file, make
/// administrators its owner and give it an access list of its own: full
/// control for administrators and the system, no inherited entries, so no
/// other account can open or change it.
///
/// A sweep by another elevated step can remove a new folder before its lock
/// is held; the creation then starts again under another name.
///
/// # Errors
/// Returns why the folder could not be created or restricted; the folder is
/// then removed.
pub fn prepare_work_folder(parent: &Path) -> Result<WorkFolder, String> {
    const ATTEMPTS: u32 = 8;
    let mut reason = String::from("cannot create the work folder");
    for attempt in 0..ATTEMPTS {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = parent.join(format!(
            "{WORK_FOLDER_PREFIX}{}-{stamp:x}{attempt}",
            std::process::id()
        ));
        if let Err(error) = std::fs::create_dir(&path) {
            reason = error.to_string();
            continue;
        }
        let lock = match hold_lock(&path.join(LOCK_NAME)) {
            Ok(lock) => lock,
            Err(error) => {
                reason = error.to_string();
                let _ = std::fs::remove_dir_all(&path);
                continue;
            }
        };
        if let Err(error) = restrict_to_administrators(&path) {
            drop(lock);
            let _ = std::fs::remove_dir_all(&path);
            return Err(error);
        }
        return Ok(WorkFolder { path, _lock: lock });
    }
    Err(reason)
}

/// Create the lock file at `path` and keep it open with only read sharing.
fn hold_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path)
}

/// Make administrators the owner of `folder`, remove every inherited and
/// every other explicit entry, and check the result.
///
/// # Errors
/// Returns why the access list could not be set, or that it is not exactly
/// the expected one after the change.
#[cfg(windows)]
pub fn restrict_to_administrators(folder: &Path) -> Result<(), String> {
    // An owner has implicit rights to change the access list. Without an
    // explicit owner, the owner is whatever the token gives new objects,
    // which policy can set to the account of the administrator. The owner
    // rights entry limits the owner of each new child to reading it.
    own_by_administrators(folder)?;
    icacls(folder, ["/inheritance:r", "/q"])?;
    icacls(
        folder,
        [
            "/grant:r",
            "*S-1-5-32-544:(OI)(CI)F",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-3-4:(OI)(CI)(IO)(R)",
            "/q",
        ],
    )?;
    // The account that created the folder can hold an explicit entry that
    // none of the grants above replaces.
    for trustee in unexpected_trustees(folder)? {
        icacls(folder, ["/remove", trustee.as_str(), "/q"])?;
    }
    let read = crate::access::read_sddls(&[folder])?;
    if read
        .first()
        .is_some_and(|sddl| work_folder_access_is_exact(sddl))
    {
        Ok(())
    } else {
        Err(format!(
            "the access list of {} is not the expected one",
            folder.display()
        ))
    }
}

/// The entries of a work folder; none is inherited.
const WORK_FOLDER_ENTRIES: [&str; 3] =
    ["(A;OICI;FA;;;BA)", "(A;OICI;FA;;;SY)", "(A;OICIIO;FR;;;OW)"];

/// Whether `sddl` names administrators as owner and holds a protected access
/// list of exactly [`WORK_FOLDER_ENTRIES`], in any order.
#[must_use]
pub fn work_folder_access_is_exact(sddl: &str) -> bool {
    let Some(rest) = sddl.trim().strip_prefix("O:BA") else {
        return false;
    };
    let Some((_, dacl)) = rest.split_once("D:") else {
        return false;
    };
    let Some(entries) = dacl.strip_prefix("PAI") else {
        return false;
    };
    let mut found: Vec<&str> = entries.split_inclusive(')').collect();
    found.sort_unstable();
    let mut expected = WORK_FOLDER_ENTRIES;
    expected.sort_unstable();
    found == expected
}

/// The SIDs of the entries of `folder` that are not for administrators, the
/// system or owner rights, as `*S-1-...` arguments for `icacls`. Windows
/// `PowerShell` resolves the account names of the entries. The path comes
/// from the environment, never from the script text.
#[cfg(windows)]
fn unexpected_trustees(folder: &Path) -> Result<Vec<String>, String> {
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
            "$ErrorActionPreference='Stop';\
             foreach($r in (Get-Acl -LiteralPath $env:CA_PATH).Access){\
             $s=$r.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;\
             if(@('S-1-5-32-544','S-1-5-18','S-1-3-4') -notcontains $s){'*'+$s}}",
        ])
        .env_clear()
        .env("SystemRoot", &root)
        .env("PSModulePath", shell.join("Modules"))
        .env("CA_PATH", folder)
        .current_dir(root.join("System32"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("cannot read the access list: {error}"))?;
    if !output.status.success() {
        return Err("cannot read the access list".to_owned());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut trustees: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.strip_prefix("*S-1-")
                .is_some_and(|rest| rest.chars().all(|c| c == '-' || c.is_ascii_digit()))
        })
        .map(str::to_owned)
        .collect();
    trustees.sort_unstable();
    trustees.dedup();
    Ok(trustees)
}

/// Make administrators the owner of `path`.
#[cfg(windows)]
fn own_by_administrators(path: &Path) -> Result<(), String> {
    icacls(path, ["/setowner", "*S-1-5-32-544", "/q"])
}

/// Replace the access list of `path` by the one its folder gives.
#[cfg(windows)]
fn inherit_access_list(path: &Path) -> Result<(), String> {
    icacls(path, ["/reset", "/q"])
}

#[cfg(windows)]
fn icacls<A: AsRef<std::ffi::OsStr>>(
    path: &Path,
    arguments: impl IntoIterator<Item = A>,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = crate::system::system_root()?;
    let system = root.join("System32");
    let status = ca_io::host_command::host_command(system.join("icacls.exe"))
        .arg(path)
        .args(arguments)
        .env_clear()
        .env("SystemRoot", &root)
        .current_dir(&system)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|error| format!("cannot set the access list: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cannot set the access list of {}", path.display()))
    }
}

/// Other platforms have no install for every user.
///
/// # Errors
/// Always.
#[cfg(not(windows))]
pub fn restrict_to_administrators(_: &Path) -> Result<(), String> {
    Err("this platform has no installer for every user".to_owned())
}

#[cfg(not(windows))]
fn own_by_administrators(_: &Path) -> Result<(), String> {
    Err("this platform has no installer for every user".to_owned())
}

#[cfg(not(windows))]
fn inherit_access_list(_: &Path) -> Result<(), String> {
    Err("this platform has no installer for every user".to_owned())
}

/// Remove the work folders of elevated steps that have ended. A folder
/// whose lock file cannot be removed belongs to a step that still runs and
/// stays; a folder without a lock file is left from a step that ended.
pub fn sweep_work_folders(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(WORK_FOLDER_PREFIX)
            || !entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        let ended = match std::fs::remove_file(entry.path().join(LOCK_NAME)) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        };
        if ended {
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
    use super::work_folder_access_is_exact as exact;

    const EXACT: &str =
        "O:BAG:S-1-5-21-1-2-3-1001D:PAI(A;OICIIO;FR;;;OW)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

    #[test]
    fn the_expected_work_folder_access_is_exact_in_any_order() {
        assert!(exact(EXACT));
        assert!(exact(
            "O:BAG:SYD:PAI(A;OICI;FA;;;BA)(A;OICIIO;FR;;;OW)(A;OICI;FA;;;SY)\r\n"
        ));
    }

    #[test]
    fn an_extra_missing_or_inherited_entry_is_not_exact() {
        assert!(!exact(&format!("{EXACT}(A;OICI;FA;;;LA)")));
        assert!(!exact(&EXACT.replace("(A;OICI;FA;;;SY)", "")));
        assert!(!exact(&EXACT.replace("D:PAI", "D:AI")));
        assert!(!exact(&EXACT.replace("D:PAI", "D:P")));
        assert!(!exact(&format!("{EXACT}(A;ID;FA;;;BU)")));
        assert!(!exact(&EXACT.replace("O:BA", "O:SY")));
        assert!(!exact(
            &EXACT.replace("(A;OICI;FA;;;BA)", "(A;OICI;FA;;;LA)")
        ));
        assert!(!exact(&EXACT.replace("FR;;;OW", "FA;;;OW")));
        assert!(!exact(""));
    }

    #[test]
    fn the_product_identity_is_the_identity_the_installer_source_gives() {
        let source = include_str!("../../../installer/compare-all.wxs");
        let code = super::UPGRADE_CODE.trim_matches(['{', '}']);
        assert!(source.contains(&format!("UpgradeCode=\"{code}\"")));
        assert!(source.contains(&format!("Name=\"{}\"", super::PRODUCT_NAME)));
    }

    #[test]
    fn the_installer_removes_an_unused_program_copy() {
        let source = include_str!("../../../installer/compare-all.wxs");
        let entry = format!(
            "<RemoveFile Id=\"RemoveStandbyExe\" Name=\"{}\" On=\"both\" />",
            super::STANDBY_NAME
        );
        assert!(source.contains(&entry));
    }
}
