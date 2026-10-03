//! The desktop program answers a version request on the console without a
//! window.
//!
//! On Windows the program has no console to write to, so the request stays a
//! refused command line there and this file holds no test.

#![cfg(not(windows))]
#![allow(
    clippy::disallowed_methods,
    reason = "the program under test is started directly, not as a host program"
)]

use std::io::Read as _;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Run the desktop program with `argument` and no display, stopping it after
/// a bound so a window that tried to open cannot hold the test.
fn run(argument: &str) -> Result<(Option<i32>, String)> {
    let settings = tempfile::tempdir()?;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_compare-all"))
        .arg(argument)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .env(ca_session::SETTINGS_DIRECTORY_VARIABLE, settings.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout)?;
    }
    Ok((status.and_then(|status| status.code()), stdout))
}

#[test]
fn the_version_switches_print_the_version_and_exit_zero() -> Result<()> {
    for argument in ["--version", "-V"] {
        let (code, stdout) = run(argument)?;
        assert_eq!(code, Some(0), "{argument}: {stdout}");
        assert_eq!(stdout, format!("{}\n", ca_app::VERSION), "{argument}");
    }
    Ok(())
}
