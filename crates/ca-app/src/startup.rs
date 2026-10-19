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

/// Serializes report writes so that a fatal reason is never replaced by a
/// routine line, whichever thread writes first.
#[derive(Debug, Default)]
pub struct ReportWriter {
    fatal_written: std::sync::Mutex<bool>,
}

impl ReportWriter {
    /// A writer that has seen no fatal reason.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fatal_written: std::sync::Mutex::new(false),
        }
    }

    /// Write a fatal reason. It replaces any earlier text, and no routine
    /// write replaces it afterwards.
    ///
    /// # Errors
    /// Returns the error of the folder or file write.
    pub fn write_fatal(&self, directory: &Path, text: &str) -> std::io::Result<PathBuf> {
        let mut fatal = self
            .fatal_written
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *fatal = true;
        write_report_in(directory, text)
    }

    /// Write a routine line unless a fatal reason was written. Returns `None`
    /// when the fatal reason kept the file.
    ///
    /// # Errors
    /// Returns the error of the folder or file write.
    pub fn write_routine(&self, directory: &Path, text: &str) -> std::io::Result<Option<PathBuf>> {
        let fatal = self
            .fatal_written
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *fatal {
            return Ok(None);
        }
        write_report_in(directory, text).map(Some)
    }
}

static WRITER: ReportWriter = ReportWriter::new();

/// Replace the report file in the settings folder with a fatal reason.
///
/// # Errors
/// Returns the error of the folder or file write.
pub fn write_fatal_report(text: &str) -> std::io::Result<PathBuf> {
    WRITER.write_fatal(&ca_ui::paths::settings_directory(), text)
}

/// Replace the report file in the settings folder with a routine line, unless
/// a fatal reason is already there.
///
/// # Errors
/// Returns the error of the folder or file write.
pub fn write_routine_report(text: &str) -> std::io::Result<Option<PathBuf>> {
    WRITER.write_routine(&ca_ui::paths::settings_directory(), text)
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

    #[test]
    fn a_routine_line_never_replaces_a_fatal_reason() -> std::io::Result<()> {
        let directory = tempfile::tempdir()?;
        let writer = ReportWriter::new();
        writer.write_fatal(directory.path(), "fatal reason")?;
        assert_eq!(writer.write_routine(directory.path(), "routine")?, None);
        let text = std::fs::read_to_string(directory.path().join(REPORT_FILE))?;
        assert_eq!(text, "fatal reason\n");
        Ok(())
    }

    #[test]
    fn a_fatal_reason_replaces_an_earlier_routine_line() -> std::io::Result<()> {
        let directory = tempfile::tempdir()?;
        let writer = ReportWriter::new();
        assert!(writer.write_routine(directory.path(), "routine")?.is_some());
        writer.write_fatal(directory.path(), "fatal reason")?;
        let text = std::fs::read_to_string(directory.path().join(REPORT_FILE))?;
        assert_eq!(text, "fatal reason\n");
        Ok(())
    }
}
