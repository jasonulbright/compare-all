//! Start-up diagnostics: when the fatal message box applies, and the file
//! that keeps the start-up result for support.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// File in the settings folder that holds the result of the last start.
pub const REPORT_FILE: &str = "startup.txt";

static WINDOW_STARTED: AtomicBool = AtomicBool::new(false);

/// Record that a window back end started and the application was created.
pub fn mark_window_started() {
    WINDOW_STARTED.store(true, Ordering::SeqCst);
}

/// True once [`mark_window_started`] ran.
#[must_use]
pub fn window_started() -> bool {
    WINDOW_STARTED.load(Ordering::SeqCst)
}

/// Whether a panic ends the program with the fatal message box.
///
/// After the window started, a build that unwinds catches panics of worker
/// threads and shows them as worker errors, so no box applies. A build that
/// aborts has no other way to tell the user why the window is gone.
#[must_use]
pub const fn shows_fatal_box(window_started: bool, aborts: bool) -> bool {
    !window_started || aborts
}

/// Path of the report file in the settings folder.
#[must_use]
pub fn report_path() -> PathBuf {
    ca_ui::paths::settings_directory().join(REPORT_FILE)
}

/// Replace the report file in `directory` with `text`. The file is readable
/// by its owner only on Unix.
///
/// # Errors
/// Returns the error of the folder or file write.
pub fn write_report_in(directory: &Path, text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(directory)?;
    let path = directory.join(REPORT_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    Ok(path)
}

/// Replace the report file in the settings folder with `text`.
///
/// # Errors
/// Returns the error of the folder or file write.
pub fn write_report(text: &str) -> std::io::Result<PathBuf> {
    write_report_in(&ca_ui::paths::settings_directory(), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_before_the_window_started_shows_the_box() {
        assert!(shows_fatal_box(false, false));
        assert!(shows_fatal_box(false, true));
    }

    #[test]
    fn a_caught_worker_panic_after_start_up_shows_nothing() {
        assert!(!shows_fatal_box(true, false));
    }

    #[test]
    fn a_panic_after_start_up_that_aborts_shows_the_box() {
        assert!(shows_fatal_box(true, true));
    }

    #[test]
    fn the_report_replaces_the_earlier_text() -> std::io::Result<()> {
        let directory = tempfile::tempdir()?;
        let first = write_report_in(directory.path(), "one")?;
        let second = write_report_in(directory.path(), "two\n")?;
        assert_eq!(first, second);
        assert_eq!(std::fs::read_to_string(&second)?, "two\n");
        Ok(())
    }
}
