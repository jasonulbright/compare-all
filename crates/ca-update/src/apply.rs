//! Apply a staged update after the program has exited.
//!
//! The program starts a copy of itself from the staging folder (the helper).
//! The helper waits until the program file can be written, which is when the
//! program has ended, checks the staged file again, applies it, optionally
//! starts the program, and ends. It registers nothing with the system.
//!
//! The staged record is untrusted input: the helper repeats the size, format,
//! SHA-256 and signer checks before it applies anything.

use crate::flow::Endpoints;
use crate::mode::{detect, InstallMode};
use crate::signature::SignatureVerifier;
use crate::staging::{AssetKind, DownloadWarning, StagedRecord, Staging};
use crate::verify::{verify_file, Expected, PORTABLE_FILES};
use crate::version::Version;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// First argument of the helper process.
pub const HELPER_ARGUMENT: &str = "--apply-update";

const WAIT_FOR_EXIT: Duration = Duration::from_secs(120);

/// Replace the program files in `target` with the files of
/// [`PORTABLE_FILES`] found in `fresh`, keeping every other file in `target`.
/// Any failure restores the earlier files.
///
/// `before_each` runs before each file is installed; an error from it counts as
/// a failure of that file.
///
/// # Errors
/// Returns why the replacement failed. The folder is then as it was, unless
/// an earlier file could not be put back: the error then names the backup
/// folder, which is kept.
pub fn apply_portable_with(
    target: &Path,
    fresh: &Path,
    before_each: &dyn Fn(&str) -> std::io::Result<()>,
) -> Result<(), String> {
    let names: Vec<&str> = PORTABLE_FILES
        .iter()
        .copied()
        .filter(|name| fresh.join(name).is_file())
        .collect();
    let backup = target.join(format!(".update-backup-{}", std::process::id()));
    std::fs::create_dir_all(&backup).map_err(|error| format!("cannot prepare backup: {error}"))?;
    // (name, had an earlier file)
    let mut done: Vec<(&str, bool)> = Vec::new();
    let result = (|| -> Result<(), String> {
        for name in &names {
            before_each(name).map_err(|error| format!("{name}: {error}"))?;
            let live = target.join(name);
            let had = live.exists();
            if had {
                std::fs::rename(&live, backup.join(name))
                    .map_err(|error| format!("cannot move {name} aside: {error}"))?;
            }
            done.push((name, had));
            let incoming = target.join(format!("{name}.new"));
            std::fs::copy(fresh.join(name), &incoming)
                .map_err(|error| format!("cannot copy {name}: {error}"))?;
            std::fs::rename(&incoming, &live)
                .map_err(|error| format!("cannot place {name}: {error}"))?;
        }
        Ok(())
    })();
    if let Err(reason) = result {
        let mut unrestored = Vec::new();
        for (name, had) in done.iter().rev() {
            let live = target.join(name);
            let _ = std::fs::remove_file(&live);
            let _ = std::fs::remove_file(target.join(format!("{name}.new")));
            if *had {
                if let Err(error) = std::fs::rename(backup.join(name), &live) {
                    unrestored.push(format!("{name} ({error})"));
                }
            }
        }
        if !unrestored.is_empty() {
            return Err(format!(
                "{reason}; could not restore {}; the earlier files are kept in {}",
                unrestored.join(", "),
                backup.display()
            ));
        }
        let _ = std::fs::remove_dir_all(&backup);
        return Err(reason);
    }
    std::fs::remove_dir_all(&backup).map_err(|error| format!("cannot remove backup: {error}"))
}

/// The command that installs `msi` as a major upgrade with a progress window
/// and no questions. A machine install writes no log: the folder a log could
/// go to is writable by the user, and the installer runs with system rights.
#[must_use]
pub fn msi_command(msi: &Path, machine: bool, log: &Path) -> Command {
    let root = crate::system::system_root().unwrap_or_else(|_| PathBuf::from(r"C:\Windows"));
    let mut command = ca_io::host_command::host_command(root.join("System32").join("msiexec.exe"));
    command.arg("/i").arg(msi).args(["/passive", "/norestart"]);
    if machine {
        command.arg("ALLUSERS=1");
    } else {
        command.arg("/L*v").arg(log);
    }
    command
}

/// Wait until `executable` can be opened for writing, which is when no process
/// runs it.
fn wait_until_free(executable: &Path, limit: Duration) -> bool {
    let started = Instant::now();
    loop {
        if std::fs::OpenOptions::new()
            .write(true)
            .open(executable)
            .is_ok()
        {
            return true;
        }
        if started.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Start the helper from a copy of the running program.
///
/// # Errors
/// Returns why the helper could not be started. The staged update is then
/// still staged.
pub fn spawn_helper(
    staging: &Staging,
    executable: &Path,
    relaunch: bool,
    arguments: &[OsString],
) -> Result<(), String> {
    let folder = staging.helper_dir()?;
    let copy = folder.join(format!("compare-all-update-{}.exe", std::process::id()));
    std::fs::copy(executable, &copy)
        .map_err(|error| format!("cannot copy the program: {error}"))?;
    let mut command = ca_io::host_command::host_command(&copy);
    command
        .arg(HELPER_ARGUMENT)
        .arg(staging.dir())
        .arg(executable)
        .arg(if relaunch { "1" } else { "0" })
        .arg("--")
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
        command.creation_flags(0x0000_0008 | 0x0800_0000 | 0x0000_0200);
    }
    command
        .spawn()
        .map(drop)
        .map_err(|error| format!("cannot start the update: {error}"))
}

/// Parsed helper arguments.
#[derive(Debug, PartialEq, Eq)]
pub struct HelperArguments {
    /// The staging folder.
    pub staging: PathBuf,
    /// The program to update.
    pub executable: PathBuf,
    /// Start the program after the update.
    pub relaunch: bool,
    /// Arguments for the restarted program.
    pub program_arguments: Vec<OsString>,
}

/// Read the helper command line (without the program name).
#[must_use]
pub fn parse_helper_arguments(arguments: &[OsString]) -> Option<HelperArguments> {
    let mut it = arguments.iter();
    if it.next()? != HELPER_ARGUMENT {
        return None;
    }
    let staging = PathBuf::from(it.next()?);
    let executable = PathBuf::from(it.next()?);
    let relaunch = it.next()? == "1";
    if it.next()? != "--" {
        return None;
    }
    Some(HelperArguments {
        staging,
        executable,
        relaunch,
        program_arguments: it.cloned().collect(),
    })
}

/// The trust anchors the helper needs.
pub struct Trust<'a> {
    /// The Authenticode verifier.
    pub signer: &'a dyn SignatureVerifier,
    /// Common name of the required signer.
    pub signer_name: &'a str,
    /// Where release pages are.
    pub endpoints: &'a Endpoints,
    /// The build that is installed now. Only a newer build is applied.
    pub running: &'a Version,
}

/// True when the program applies a staged update of a copy installed this
/// way. An install for every user is never applied by the program: that
/// needs an elevated installer, and the update is offered as a download page
/// instead.
#[must_use]
pub const fn applies_automatically(mode: InstallMode) -> bool {
    !matches!(mode, InstallMode::Installed { machine: true })
}

/// Apply the staged update. Returns the version installed.
///
/// # Errors
/// Returns why the update was not applied. The caller leaves the installed
/// version as it is.
pub fn apply_staged(
    staging: &Staging,
    mode: InstallMode,
    trust: &Trust<'_>,
    install: &dyn Fn(Command) -> Result<(), String>,
    before_each: &dyn Fn(&str) -> std::io::Result<()>,
    target_dir: &Path,
) -> Result<Version, String> {
    if !applies_automatically(mode) {
        return Err("an install for every user is not updated by the program".to_owned());
    }
    let record = staging
        .read_record()
        .ok_or("no valid staged update exists")?;
    let version = record
        .parsed_version()
        .ok_or("the staged version is invalid")?;
    if version <= *trust.running {
        return Err(format!(
            "the staged version {version} is not newer than {}",
            trust.running
        ));
    }
    let kind = record
        .kind()
        .ok_or("the staged file is not a release file")?;
    let wanted = match mode {
        InstallMode::Installed { .. } => AssetKind::Msi,
        InstallMode::Portable => AssetKind::Zip,
    };
    if kind != wanted {
        return Err("the staged file does not fit how the program is installed".to_owned());
    }
    let scratch = staging.dir().join("scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    let expected = Expected {
        kind,
        size: record.size,
        sha256: &record.sha256,
        signer: trust.signer_name,
        version: &version,
    };
    let file = staging.asset_path(&record);
    let checked = verify_file(&file, &expected, trust.signer, &scratch);
    let result = checked.and_then(|()| match mode {
        InstallMode::Installed { machine } => {
            let log = staging.dir().join("installer.log");
            install(msi_command(&file, machine, &log))
        }
        InstallMode::Portable => apply_portable_with(target_dir, &scratch, before_each),
    });
    let _ = std::fs::remove_dir_all(&scratch);
    result.map(|()| version)
}

fn run_installer(mut command: Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("cannot start the installer: {error}"))?;
    match status.code() {
        Some(0 | 1641 | 3010) => Ok(()),
        Some(code) => Err(format!("the installer ended with code {code}")),
        None => Err("the installer was stopped".to_owned()),
    }
}

/// The entry point of the helper process. Returns the exit code.
///
/// A helper that cannot claim the staged update ends at once and changes
/// nothing: another helper applies it.
#[must_use]
pub fn helper_main(
    arguments: &HelperArguments,
    signer: &dyn SignatureVerifier,
    signer_name: &str,
    running: &Version,
) -> i32 {
    let staging = Staging::new(arguments.staging.clone());
    let Ok(claim) = staging.begin_apply() else {
        return 2;
    };
    let endpoints = Endpoints::github();
    let trust = Trust {
        signer,
        signer_name,
        endpoints: &endpoints,
        running,
    };
    let mode = detect(&arguments.executable);
    let target = arguments
        .executable
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let outcome = if wait_until_free(&arguments.executable, WAIT_FOR_EXIT) {
        apply_staged(&staging, mode, &trust, &run_installer, &|_| Ok(()), &target)
    } else {
        Err("the program did not end".to_owned())
    };
    let code = match &outcome {
        Ok(_) => 0,
        Err(_) => 1,
    };
    if let Err(reason) = &outcome {
        staging.write_apply_failure(reason);
        if let Some(version) = staging.read_record().and_then(|r| r.parsed_version()) {
            let _ = staging.write_warning(&DownloadWarning::new(
                &version,
                trust.endpoints.page(&version),
            ));
        }
    }
    staging.clear();
    drop(claim);
    if arguments.relaunch {
        let _ = ca_io::host_command::host_command(&arguments.executable)
            .args(&arguments.program_arguments)
            .spawn();
    }
    code
}

/// True when a verified staged update waits.
#[must_use]
pub fn has_staged(staging: &Staging) -> bool {
    staging
        .read_record()
        .is_some_and(|record: StagedRecord| record.verified)
}
