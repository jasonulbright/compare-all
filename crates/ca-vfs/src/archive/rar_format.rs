//! RAR, read only, through the system `bsdtar`.
//!
//! No pure Rust RAR decoder exists, so the container is handed to `bsdtar`,
//! which converts it to a pax tar on its standard output. The tar lands in a
//! temporary file and is then read like any other tar. One child process runs
//! per container opened.
//!
//! The child is started from an argument array, never through a shell. The
//! only argument that carries outside text is the container path, and it is
//! passed as `@<path>`, which `bsdtar` never reads as an option. The child
//! runs under a deadline and an output ceiling, and a raised cancellation
//! flag kills it.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::ArchiveBacking;
use crate::cancel::Cancel;
use crate::error::{LimitKind, VfsError, VfsResult};
use crate::limits::Limits;

/// Most bytes kept from the child's error stream.
const STDERR_BYTES: u64 = 16 * 1024;
/// Most bytes the version probe may print.
const VERSION_BYTES: u64 = 16 * 1024;
/// How often the parent checks the child, the deadline and the flag.
const POLL: Duration = Duration::from_millis(20);

/// Where this platform keeps a `bsdtar`, or why it has none.
fn candidate() -> Result<PathBuf, String> {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot")
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        return Ok(root.join("System32").join("tar.exe"));
    }
    if cfg!(target_os = "macos") {
        return Ok(PathBuf::from("/usr/bin/tar"));
    }
    let path = ca_io::host_command::host_variable("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("bsdtar"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            "RAR archives are read through bsdtar, which is not installed. Install the \
             libarchive-tools package (Debian, Ubuntu) or the bsdtar package (Fedora)."
                .to_owned()
        })
}

/// The verified `bsdtar`, found once per process.
fn tool() -> VfsResult<&'static Path> {
    static TOOL: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let found = TOOL.get_or_init(|| {
        let program = candidate()?;
        if !program.is_file() {
            return Err(format!(
                "RAR archives are read through the system tar (bsdtar), which is not at {}",
                program.display()
            ));
        }
        let output = run(
            &program,
            &[OsString::from("--version")],
            Vec::new(),
            VERSION_BYTES,
            Duration::from_secs(60),
            &Cancel::new(),
        )
        .map_err(|error| format!("{} --version failed: {error}", program.display()))?;
        if String::from_utf8_lossy(&output).contains("bsdtar") {
            Ok(program)
        } else {
            Err(format!(
                "{} is not bsdtar; only bsdtar reads RAR archives",
                program.display()
            ))
        }
    });
    match found {
        Ok(program) => Ok(program.as_path()),
        Err(reason) => Err(VfsError::unsupported(reason.clone())),
    }
}

/// Convert the RAR container into a pax tar held in a temporary file.
///
/// # Errors
/// Returns [`VfsError::Unsupported`] when no usable `bsdtar` exists,
/// [`VfsError::Corrupt`] when `bsdtar` rejects the container,
/// [`VfsError::LimitExceeded`] when the tar passes the container ceiling,
/// [`VfsError::Timeout`] when the child passes its deadline, and
/// [`VfsError::Cancelled`] when the flag is raised.
pub(crate) fn convert(
    backing: &ArchiveBacking,
    limits: &Limits,
    cancel: &Cancel,
) -> VfsResult<ArchiveBacking> {
    let program = tool()?;
    let (path, _keep) = on_disk(backing)?;
    let mut source = OsString::from("@");
    source.push(path.as_os_str());
    let args = [
        OsString::from("-c"),
        OsString::from("-f"),
        OsString::from("-"),
        OsString::from("--format=pax"),
        source,
    ];
    let temp = tempfile::NamedTempFile::new()?;
    let temp = run(
        program,
        &args,
        temp,
        limits.max_archive_bytes,
        Duration::from_secs(limits.max_helper_seconds),
        cancel,
    )?;
    Ok(ArchiveBacking::Temp(Arc::new(temp)))
}

/// A path the child can open, writing an in-memory container out first.
fn on_disk(backing: &ArchiveBacking) -> VfsResult<(PathBuf, Option<tempfile::NamedTempFile>)> {
    if let Some(path) = backing.path() {
        // An absolute path cannot begin with "-", so "@<path>" never reads as
        // "@-", which names standard input.
        return Ok((std::path::absolute(path)?, None));
    }
    let mut temp = tempfile::NamedTempFile::new()?;
    let mut reader = backing.reader()?;
    std::io::copy(&mut reader, &mut temp)?;
    temp.flush()?;
    Ok((temp.path().to_path_buf(), Some(temp)))
}

/// Run `program` with `args`, copying its standard output into `sink`.
///
/// # Errors
/// Returns [`VfsError::LimitExceeded`] past `cap` output bytes,
/// [`VfsError::Timeout`] past `deadline`, [`VfsError::Cancelled`] when the
/// flag is raised, and [`VfsError::Corrupt`] with the child's error text when
/// it exits with a failure.
fn run<W: Write + Send + 'static>(
    program: &Path,
    args: &[OsString],
    sink: W,
    cap: u64,
    deadline: Duration,
    cancel: &Cancel,
) -> VfsResult<W> {
    let mut command = ca_io::host_command::host_command(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut command);
    let mut child = command.spawn()?;

    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let flag = Arc::clone(&overflow);
    let out_thread = std::thread::spawn(move || copy_capped(stdout, sink, cap, &flag));
    let err_thread = std::thread::spawn(move || {
        // The stream is drained to its end even past the kept prefix: a child
        // blocked on a full error pipe would otherwise hang until the deadline.
        let mut text = Vec::new();
        if let Some(mut stream) = stderr {
            let mut chunk = [0u8; 4096];
            while let Ok(read) = stream.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let room = usize::try_from(STDERR_BYTES)
                    .unwrap_or(usize::MAX)
                    .saturating_sub(text.len());
                text.extend_from_slice(chunk.get(..read.min(room)).unwrap_or_default());
            }
        }
        text
    });

    let started = Instant::now();
    let outcome = loop {
        if cancel.is_cancelled() {
            break Err(VfsError::Cancelled);
        }
        if overflow.load(Ordering::SeqCst) {
            break Err(VfsError::LimitExceeded {
                kind: LimitKind::ArchiveSize,
                limit: cap,
            });
        }
        if started.elapsed() > deadline {
            break Err(VfsError::timeout("bsdtar"));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(POLL),
            Err(error) => break Err(VfsError::Io(error)),
        }
    };
    let status = match outcome {
        Ok(status) => status,
        Err(error) => {
            stop(&mut child);
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Err(error);
        }
    };

    let copied = out_thread
        .join()
        .map_err(|_| VfsError::corrupt("bsdtar output reader stopped"))?;
    let text = err_thread.join().unwrap_or_default();
    if overflow.load(Ordering::SeqCst) {
        return Err(VfsError::LimitExceeded {
            kind: LimitKind::ArchiveSize,
            limit: cap,
        });
    }
    let sink = copied?;
    if !status.success() {
        let text = String::from_utf8_lossy(&text);
        let first = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        return Err(VfsError::corrupt(format!(
            "bsdtar could not read the archive: {}",
            first.trim()
        )));
    }
    Ok(sink)
}

/// Copy the child's output into `sink` until the end or until `cap` bytes.
fn copy_capped<W: Write>(
    stream: Option<std::process::ChildStdout>,
    mut sink: W,
    cap: u64,
    overflow: &AtomicBool,
) -> VfsResult<W> {
    let Some(mut stream) = stream else {
        return Ok(sink);
    };
    let mut total = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(VfsError::Io(error)),
        };
        total = total.saturating_add(read as u64);
        if total > cap {
            overflow.store(true, Ordering::SeqCst);
            return Ok(sink);
        }
        sink.write_all(chunk.get(..read).unwrap_or_default())?;
    }
    sink.flush()?;
    Ok(sink)
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let error = run(
            Path::new("definitely-not-a-program-here"),
            &[],
            Vec::new(),
            16,
            Duration::from_secs(5),
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(matches!(error, VfsError::Io(_)));
    }

    #[test]
    fn a_raised_flag_stops_the_child() {
        let Ok(program) = tool() else {
            return;
        };
        let cancel = Cancel::new();
        cancel.cancel();
        let error = run(
            program,
            &[OsString::from("--version")],
            Vec::new(),
            VERSION_BYTES,
            Duration::from_secs(60),
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(error, VfsError::Cancelled));
    }

    #[test]
    fn output_past_the_cap_is_a_limit() {
        let Ok(program) = tool() else {
            return;
        };
        let error = run(
            program,
            &[OsString::from("--version")],
            Vec::new(),
            4,
            Duration::from_secs(60),
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(matches!(error, VfsError::LimitExceeded { .. }));
    }
}
