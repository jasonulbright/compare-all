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
use crate::staging::{ApplyClaim, AssetKind, DownloadWarning, StagedRecord, Staging};
use crate::verify::{hold_read_only, verify_and_hold, Expected, Unpacked};
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
    apply_unpacked(target, &Unpacked::hold_folder(fresh)?, before_each)
}

/// [`apply_portable_with`] for files held open since their check. Each copy
/// must have the bytes that were checked.
///
/// # Errors
/// As [`apply_portable_with`].
pub fn apply_unpacked(
    target: &Path,
    fresh: &Unpacked,
    before_each: &dyn Fn(&str) -> std::io::Result<()>,
) -> Result<(), String> {
    let names: Vec<&str> = fresh.names().collect();
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
            fresh.copy_to(name, &incoming)?;
            std::fs::rename(&incoming, &live)
                .map_err(|error| format!("cannot place {name}: {error}"))?;
        }
        Ok(())
    })();
    if let Err(reason) = result {
        let mut unrestored = Vec::new();
        for (name, had) in done.iter().rev() {
            let live = target.join(name);
            let _ = std::fs::remove_file(target.join(format!("{name}.new")));
            if !*had {
                let _ = std::fs::remove_file(&live);
            } else if let Err(error) = restore(&backup.join(name), &live) {
                unrestored.push(format!("{name} ({error})"));
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
    // Every file is in place. A backup folder that cannot be removed now is
    // left behind; it is not a failed update.
    let _ = std::fs::remove_dir_all(&backup);
    Ok(())
}

/// Put `saved` back at `live`. The rename replaces whatever `live` holds, so
/// the new file stays in place until the earlier one is back: a restore that
/// fails never leaves the folder without the file. Short locks, such as a
/// scan of the new file, are waited out; a backup that cannot be moved is
/// copied.
fn restore(saved: &Path, live: &Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = 5;
    let mut attempt = 1;
    loop {
        match std::fs::rename(saved, live) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return std::fs::copy(saved, live).map(drop),
        }
    }
}

/// The reason [`apply_staged`] gives when the elevation of an install for
/// every user did not happen: the prompt was declined, could not be
/// approved, was refused by policy, or the elevated program did not start.
pub const ELEVATION_DECLINED: &str = "the elevation was declined or failed";

/// The reason [`apply_staged`] gives when the system installer was busy with
/// another install. The staged update stays for the next close.
pub const INSTALLER_BUSY: &str = "another install is in progress";

/// The reason [`apply_staged`] gives when the elevated step changed nothing
/// and kept the staged update: it could not set the program file aside, or
/// another elevated step held the install folder. The update stays for the
/// next close.
pub const INSTALL_PUT_OFF: &str = "the install for every user was put off";

/// The reason [`apply_staged`] gives when this install cannot apply an
/// update by itself: its folder or program file is open to other accounts,
/// or its staging folder is outside every user profile. The staged update is
/// removed, the download warning shows, and that release is not downloaded
/// again.
pub const CANNOT_APPLY_HERE: &str =
    "updates cannot be applied to this install automatically; install the release from its page";

/// `ERROR_INSTALL_ALREADY_RUNNING`.
const INSTALLER_BUSY_CODE: i32 = 1618;

/// `ERROR_CANCELLED`, the code of a declined elevation prompt.
const DECLINED_CODE: i32 = 1223;

/// `ERROR_INSTALL_USEREXIT`: Cancel in the progress window of the installer.
const INSTALL_CANCELLED_CODE: i32 = 1602;

/// The elevation script ends with this bit set when the elevated program
/// never started; the low 16 bits then hold the Windows error, or 0 when
/// none was given. Windows status codes of a crashed process never set it.
const NOT_STARTED: i32 = 0x2000_0000;

/// Starts the installed program (absolute path in `CA_PROGRAM`) through the
/// standard elevation prompt with the install argument and the staging
/// folder (`CA_UPDATE_DIR`), waits for it, and ends with its exit code. The
/// paths come from the environment, never from the script text.
///
/// Windows `PowerShell` 5.1 `Start-Process` replaces the error of a failed
/// start with one that carries no error code, so the start goes through
/// `Process.Start` and the error code is read from the inner exception.
const ELEVATE_SCRIPT: &str = "$ErrorActionPreference='Stop';\
    try{$i=[Diagnostics.ProcessStartInfo]::new($env:CA_PROGRAM);\
    $i.Arguments='--install-update \"'+$env:CA_UPDATE_DIR.TrimEnd('\\')+'\"';\
    $i.UseShellExecute=$true;$i.Verb='runas';\
    $i.WorkingDirectory=$env:SystemRoot+'\\System32';\
    $p=[Diagnostics.Process]::Start($i)}\
    catch{$e=$_.Exception;\
    while($null -ne $e -and $e -isnot [ComponentModel.Win32Exception]){$e=$e.InnerException};\
    if($null -ne $e){exit (0x20000000 -bor ($e.NativeErrorCode -band 0xFFFF))};exit 0x20000000}\
    if($null -eq $p){exit 0x20000000};$p.WaitForExit();exit $p.ExitCode";

/// The Windows error of an elevation that never started, from the exit code
/// of the elevation script; `Some(0)` when the start failed without one.
#[must_use]
pub fn elevation_error(code: Option<i32>) -> Option<i32> {
    code.filter(|code| code & !0xFFFF == NOT_STARTED)
        .map(|code| code & 0xFFFF)
}

/// The command that installs `msi` for one user as a major upgrade with a
/// progress window, no questions and a log at `log`.
#[must_use]
pub fn msi_command(msi: &Path, log: &Path) -> Command {
    let root = crate::system::system_root().unwrap_or_else(|_| PathBuf::from(r"C:\Windows"));
    let mut command = ca_io::host_command::host_command(root.join("System32").join("msiexec.exe"));
    command
        .arg("/i")
        .arg(msi)
        .args(["/passive", "/norestart", "/L*v"])
        .arg(log);
    command
}

/// The command that updates an install for every user: it starts `program`,
/// the installed program, through the standard elevation prompt to install
/// the update staged in `staging`. The prompt names that signed program. It
/// writes no log.
#[must_use]
pub fn elevate_command(program: &Path, staging: &Path) -> Command {
    let root = crate::system::system_root().unwrap_or_else(|_| PathBuf::from(r"C:\Windows"));
    let shell = root.join("System32").join("WindowsPowerShell").join("v1.0");
    let mut command = ca_io::host_command::host_command(shell.join("powershell.exe"));
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-NoLogo",
            "-Command",
            ELEVATE_SCRIPT,
        ])
        .env_clear()
        .env("SystemRoot", &root)
        .env("PSModulePath", shell.join("Modules"))
        .current_dir(root.join("System32"));
    // The elevation prompt expands %SystemDrive% for its caches; unset, the
    // text is used as a folder name below the working folder.
    if let Some(std::path::Component::Prefix(drive)) = root.components().next() {
        command.env("SystemDrive", drive.as_os_str());
    }
    command
        .env("CA_PROGRAM", program)
        .env("CA_UPDATE_DIR", staging);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// What the exit code of an installer command means.
///
/// # Errors
/// [`ELEVATION_DECLINED`] when the elevation did not happen or the user
/// cancelled the installer, [`INSTALLER_BUSY`] when another install held the
/// installer, [`INSTALL_PUT_OFF`] when the elevated step kept the update for
/// a later close; otherwise why the install failed.
pub fn installer_result(code: Option<i32>) -> Result<(), String> {
    if elevation_error(code).is_some() {
        return Err(ELEVATION_DECLINED.to_owned());
    }
    match code {
        Some(0 | 1641 | 3010) => Ok(()),
        Some(DECLINED_CODE | INSTALL_CANCELLED_CODE) => Err(ELEVATION_DECLINED.to_owned()),
        Some(INSTALLER_BUSY_CODE) => Err(INSTALLER_BUSY.to_owned()),
        Some(crate::install::REFUSED) => {
            Err("the installer failed its check before the install for every user".to_owned())
        }
        Some(crate::install::KEPT) => Err(INSTALL_PUT_OFF.to_owned()),
        Some(crate::install::NOT_APPLICABLE) => Err(CANNOT_APPLY_HERE.to_owned()),
        Some(crate::install::NOT_PREPARED) => {
            Err("the install for every user could not be prepared".to_owned())
        }
        Some(code) => Err(format!("the installer ended with code {code}")),
        None => Err("the installer was stopped".to_owned()),
    }
}

/// Wait until `executable` can be opened for writing, which is when no process
/// runs it. Only a sharing violation means the program still runs; any other
/// error ends the wait at once.
fn wait_until_free(executable: &Path, limit: Duration) -> Result<(), String> {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let started = Instant::now();
    loop {
        match std::fs::OpenOptions::new().write(true).open(executable) {
            Ok(_) => return Ok(()),
            Err(error) if error.raw_os_error() != Some(ERROR_SHARING_VIOLATION) => {
                return Err(format!("cannot write the program file: {error}"));
            }
            Err(_) if started.elapsed() > limit => {
                return Err("the program did not end".to_owned());
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// Start the helper from a copy of the running program. The copy stays held
/// against writing, renaming and deleting from its write until the helper
/// runs from it, so the program that starts is the program that was copied.
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
    let held = crate::verify::copy_held(executable, &copy)
        .map_err(|error| format!("cannot copy the program: {error}"))?;
    let mut command = ca_io::host_command::host_command(&copy);
    command
        .arg(HELPER_ARGUMENT)
        .arg(staging.dir())
        .arg(executable)
        .arg(if relaunch { "1" } else { "0" })
        .arg("--")
        .args(arguments)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
        command.creation_flags(0x0000_0008 | 0x0800_0000 | 0x0000_0200);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start the update: {error}"))?;
    drop(held);
    // The writing end of the helper's input stays open until this process
    // ends; the helper of an install for every user waits for that end.
    std::mem::forget(child.stdin.take());
    Ok(())
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

/// Apply the staged update. Returns the version installed.
///
/// An installer stays open with writing, renaming and deleting refused from
/// before its check until the installer has ended. An install for every user
/// starts the installed program elevated, which installs from a copy of its
/// own (see [`crate::install`]); `target_dir` is the install folder.
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
    let scratch = staging.scratch_dir();
    let _ = std::fs::remove_dir_all(&scratch);
    let expected = Expected {
        kind,
        size: record.size,
        sha256: &record.sha256,
        signer: trust.signer_name,
        version: &version,
    };
    let file = staging.asset_path(&record);
    let _installer = match kind {
        AssetKind::Msi => Some(
            hold_read_only(&file)
                .map_err(|error| format!("cannot open the staged installer: {error}"))?,
        ),
        AssetKind::Zip => None,
    };
    let result =
        verify_and_hold(&file, &expected, trust.signer, &scratch).and_then(|held| {
            match (mode, held) {
                (InstallMode::Installed { machine: true }, _) => install(elevate_command(
                    &target_dir.join(crate::install::PROGRAM_NAME),
                    staging.dir(),
                )),
                (InstallMode::Installed { machine: false }, _) => {
                    install(msi_command(&file, &staging.dir().join("installer.log")))
                }
                (InstallMode::Portable, Some(unpacked)) => {
                    apply_unpacked(target_dir, &unpacked, before_each)
                }
                (InstallMode::Portable, None) => {
                    Err("the staged file is not an archive".to_owned())
                }
            }
        });
    let _ = std::fs::remove_dir_all(&scratch);
    result.map(|()| version)
}

fn run_installer(mut command: Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("cannot start the installer: {error}"))?;
    installer_result(status.code())
}

/// Record what one apply did and release the staging folder. Returns the
/// exit code of the helper.
///
/// A declined elevation, a busy installer or an install that was put off
/// changes nothing: the staged
/// update stays and is offered again at the next close, with no warning. Any
/// other failure keeps the reason and the download warning and removes the
/// staged update.
#[must_use]
pub fn finish_apply(
    staging: &Staging,
    claim: &ApplyClaim,
    outcome: &Result<Version, String>,
    endpoints: &Endpoints,
) -> i32 {
    match outcome {
        Ok(_) => {
            staging.clear_claimed(claim);
            0
        }
        Err(reason) if reason == ELEVATION_DECLINED => 3,
        Err(reason) if reason == INSTALLER_BUSY || reason == INSTALL_PUT_OFF => 4,
        Err(reason) => {
            staging.write_apply_failure(reason);
            if let Some(version) = staging.read_record().and_then(|r| r.parsed_version()) {
                let _ = staging
                    .write_warning(&DownloadWarning::new(&version, endpoints.page(&version)));
            }
            staging.clear_claimed(claim);
            1
        }
    }
}

/// The entry point of the helper process. Returns the exit code.
#[must_use]
pub fn helper_main(
    arguments: &HelperArguments,
    signer: &dyn SignatureVerifier,
    signer_name: &str,
    running: &Version,
) -> i32 {
    helper_main_waiting(arguments, signer, signer_name, running, WAIT_FOR_EXIT)
}

/// [`helper_main`] that waits at most `wait` for the claim on the staging
/// folder, which a download in another run of the program or another helper
/// may hold. A helper that gets no claim changes nothing, reports no failure
/// and does not start the program: an apply that still runs elsewhere may
/// need the program files free. A helper that gets the claim and finds
/// nothing staged, because another helper applied the update, starts the
/// program when asked to. The program is started again only after the
/// program that started the helper has ended.
#[must_use]
pub fn helper_main_waiting(
    arguments: &HelperArguments,
    signer: &dyn SignatureVerifier,
    signer_name: &str,
    running: &Version,
    wait: Duration,
) -> i32 {
    helper_main_with(arguments, signer, signer_name, running, wait, &|| {
        let _ = ca_io::host_command::host_command(&arguments.executable)
            .args(&arguments.program_arguments)
            .spawn();
    })
}

/// [`helper_main_waiting`] that calls `start_program` to start the program
/// again.
#[must_use]
pub fn helper_main_with(
    arguments: &HelperArguments,
    signer: &dyn SignatureVerifier,
    signer_name: &str,
    running: &Version,
    wait: Duration,
    start_program: &dyn Fn(),
) -> i32 {
    let staging = Staging::new(arguments.staging.clone());
    let relaunch = || {
        if arguments.relaunch {
            start_program();
        }
    };
    let started = Instant::now();
    let claim = loop {
        match staging.begin_apply() {
            Ok(claim) => break claim,
            Err(_) if started.elapsed() < wait => std::thread::sleep(Duration::from_millis(250)),
            Err(_) => return 2,
        }
    };
    let mode = detect(&arguments.executable);
    let wait_for_end = || match mode {
        InstallMode::Installed { machine: true } => wait_for_parent(WAIT_FOR_EXIT),
        _ => wait_until_free(&arguments.executable, WAIT_FOR_EXIT),
    };
    if staging.read_record().is_none() {
        // Another helper applied the update and released the claim.
        drop(claim);
        if wait_for_end().is_ok() {
            relaunch();
        }
        return 2;
    }
    let endpoints = Endpoints::github();
    let trust = Trust {
        signer,
        signer_name,
        endpoints: &endpoints,
        running,
    };
    let target = arguments
        .executable
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let ended = wait_for_end();
    let program_ended = ended.is_ok();
    let outcome = ended.and_then(|()| {
        if mode == (InstallMode::Installed { machine: true })
            && !crate::install::install_place_accepted(staging.dir(), &arguments.executable)
        {
            return Err(CANNOT_APPLY_HERE.to_owned());
        }
        apply_staged(&staging, mode, &trust, &run_installer, &|_| Ok(()), &target)
    });
    let code = finish_apply(&staging, &claim, &outcome, &endpoints);
    drop(claim);
    // A program that did not end is still open; a second copy is not started.
    if program_ended {
        relaunch();
    }
    code
}

/// Wait until the program that started the helper has ended. A user without
/// rights cannot open a program file of an install for every user for
/// writing, so the end is seen on standard input instead: the program keeps
/// the writing end of that pipe open until it ends.
fn wait_for_parent(limit: Duration) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        let _ = sender.send(());
    });
    receiver
        .recv_timeout(limit)
        .map_err(|_| "the program did not end".to_owned())
}

/// True when a verified staged update waits.
#[must_use]
pub fn has_staged(staging: &Staging) -> bool {
    staging
        .read_record()
        .is_some_and(|record: StagedRecord| record.verified)
}
