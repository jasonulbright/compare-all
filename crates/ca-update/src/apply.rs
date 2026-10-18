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
use crate::verify::{verify_file, Expected};
use crate::version::Version;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// First argument of the helper process.
pub const HELPER_ARGUMENT: &str = "--apply-update";

const WAIT_FOR_EXIT: Duration = Duration::from_secs(120);

/// Replace the program files in `target` with the files in `fresh`, keeping
/// every other file in `target`. Any failure restores the earlier files.
///
/// `before_each` runs before each file is installed; an error from it counts as
/// a failure of that file.
///
/// # Errors
/// Returns why the replacement failed. The folder is then as it was.
pub fn apply_portable_with(
    target: &Path,
    fresh: &Path,
    before_each: &dyn Fn(&str) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut names: Vec<OsString> = std::fs::read_dir(fresh)
        .map_err(|error| error.to_string())?
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    let backup = target.join(format!(".update-backup-{}", std::process::id()));
    std::fs::create_dir_all(&backup).map_err(|error| format!("cannot prepare backup: {error}"))?;
    // (name, had an earlier file)
    let mut done: Vec<(OsString, bool)> = Vec::new();
    let result = (|| -> Result<(), String> {
        for name in &names {
            let label = name.to_string_lossy().into_owned();
            before_each(&label).map_err(|error| format!("{label}: {error}"))?;
            let live = target.join(name);
            let had = live.exists();
            if had {
                std::fs::rename(&live, backup.join(name))
                    .map_err(|error| format!("cannot move {label} aside: {error}"))?;
            }
            done.push((name.clone(), had));
            let incoming = target.join(format!("{label}.new"));
            std::fs::copy(fresh.join(name), &incoming)
                .map_err(|error| format!("cannot copy {label}: {error}"))?;
            std::fs::rename(&incoming, &live)
                .map_err(|error| format!("cannot place {label}: {error}"))?;
        }
        Ok(())
    })();
    if let Err(reason) = result {
        for (name, had) in done.iter().rev() {
            let live = target.join(name);
            let _ = std::fs::remove_file(&live);
            let _ = std::fs::remove_file(target.join(format!("{}.new", name.to_string_lossy())));
            if *had {
                let _ = std::fs::rename(backup.join(name), &live);
            }
        }
        let _ = std::fs::remove_dir_all(&backup);
        return Err(reason);
    }
    std::fs::remove_dir_all(&backup).map_err(|error| format!("cannot remove backup: {error}"))
}

/// The command that installs `msi` as a major upgrade with a progress window
/// and no questions.
#[must_use]
pub fn msi_command(msi: &Path, machine: bool, log: &Path) -> Command {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
    let mut command =
        ca_io::host_command::host_command(Path::new(&root).join("System32").join("msiexec.exe"));
    command
        .arg("/i")
        .arg(msi)
        .args(["/passive", "/norestart", "/L*v"])
        .arg(log);
    if machine {
        command.arg("ALLUSERS=1");
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
    let record = staging
        .read_record()
        .ok_or("no valid staged update exists")?;
    let version = record
        .parsed_version()
        .ok_or("the staged version is invalid")?;
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
#[must_use]
pub fn helper_main(
    arguments: &HelperArguments,
    signer: &dyn SignatureVerifier,
    signer_name: &str,
) -> i32 {
    let staging = Staging::new(arguments.staging.clone());
    let endpoints = Endpoints::github();
    let trust = Trust {
        signer,
        signer_name,
        endpoints: &endpoints,
    };
    let mode = detect(&arguments.executable);
    let target = arguments
        .executable
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let _ = staging.begin_apply();
    let outcome = if wait_until_free(&arguments.executable, WAIT_FOR_EXIT) {
        apply_staged(&staging, mode, &trust, &run_installer, &|_| Ok(()), &target)
    } else {
        Err("the program did not end".to_owned())
    };
    let code = match &outcome {
        Ok(_) => 0,
        Err(_) => 1,
    };
    if let Err(_reason) = &outcome {
        if let Some(version) = staging.read_record().and_then(|r| r.parsed_version()) {
            let _ = staging.write_warning(&DownloadWarning::new(
                &version,
                trust.endpoints.page(&version),
            ));
        }
    }
    staging.clear();
    staging.end_apply();
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
