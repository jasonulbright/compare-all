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
use crate::version::Version;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// First argument of the elevated program.
pub const INSTALL_ARGUMENT: &str = "--install-update";

/// The file name of the installed program.
pub const PROGRAM_NAME: &str = "compare-all.exe";

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
    let Ok(parent) = crate::system::program_files() else {
        return NOT_PREPARED;
    };
    sweep_work_folders(&parent);
    let Ok(work) = prepare_work_folder(&parent) else {
        return NOT_PREPARED;
    };
    let code = install_from(
        &Staging::new(staging.to_path_buf()),
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
    if verify_and_hold(&copy, &expected, checks.signer, &work.join("scratch")).is_err() {
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

/// Copy `source` to `destination`, a new file, and fail unless `source` held
/// exactly `size` bytes.
fn copy_at_most(source: &Path, destination: &Path, size: u64) -> Result<(), String> {
    use std::io::Read;
    let input = std::fs::File::open(source).map_err(|error| error.to_string())?;
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
