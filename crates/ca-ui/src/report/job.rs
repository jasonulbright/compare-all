//! Writing a report on a worker.
//!
//! The document streams to a temporary file beside the target and the file is
//! renamed over the target once the last byte is written. A run that stops, or
//! that the engine refuses, removes the temporary file, so the name the user
//! chose either holds a whole report or holds what it held before.

use super::plan::{ReportPlan, Target};
use crate::worker::{Cancel, Job, Terminal};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

/// What the report job posts back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportMessage {
    /// The document is being written.
    Progress {
        /// Bytes written so far.
        bytes: u64,
    },
    /// The report was written to a file.
    Written {
        /// Where it was written.
        path: PathBuf,
        /// How large it is.
        bytes: u64,
    },
    /// The report was produced as text for the clipboard.
    Copied {
        /// The document.
        text: String,
    },
    /// The run stopped before the document was finished.
    Cancelled,
    /// The report could not be written.
    Failed {
        /// What to tell the user.
        reason: String,
    },
}

impl Terminal for ReportMessage {
    fn is_terminal(&self) -> bool {
        !matches!(self, Self::Progress { .. })
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed { reason: detail }
    }
}

/// How many bytes are written between two progress messages.
const PROGRESS_STRIDE: u64 = 64 * 1024;

/// Start writing the report described by `plan`.
///
/// Dropping the returned handle raises the flag, so a newer request supersedes
/// an older one by replacing the handle the view holds.
#[must_use]
pub fn spawn(plan: ReportPlan, notify: Arc<dyn Fn() + Send + Sync>) -> Job<ReportMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let message = match plan.settings.target {
                Target::Clipboard => to_clipboard(&plan, cancel),
                _ => to_file(&plan, cancel, &|bytes| {
                    emitter.send(ReportMessage::Progress { bytes });
                }),
            };
            emitter.send(message);
        },
        notify,
    )
}

fn to_clipboard(plan: &ReportPlan, cancel: &Cancel) -> ReportMessage {
    let mut buffer: Vec<u8> = Vec::new();
    match plan.write(&mut buffer, cancel) {
        Ok(()) => ReportMessage::Copied {
            text: String::from_utf8_lossy(&buffer).into_owned(),
        },
        Err(ca_report::ReportError::Cancelled) => ReportMessage::Cancelled,
        Err(other) => ReportMessage::Failed {
            reason: other.to_string(),
        },
    }
}

fn to_file(plan: &ReportPlan, cancel: &Cancel, progress: &dyn Fn(u64)) -> ReportMessage {
    let target = PathBuf::from(plan.settings.path.trim());
    if target.as_os_str().is_empty() {
        return ReportMessage::Failed {
            reason: "Name the file the report is written to.".to_owned(),
        };
    }
    let outcome = ca_io::replace_checked(
        &target,
        |writer| {
            let mut counter = Counting {
                inner: std::io::BufWriter::new(writer),
                written: 0,
                reported: 0,
                progress,
            };
            plan.write(&mut counter, cancel)?;
            counter.flush()?;
            Ok(counter.written)
        },
        || {
            if cancel.is_cancelled() {
                Err(ca_report::ReportError::Cancelled)
            } else {
                Ok(())
            }
        },
    );
    match outcome {
        Ok(written) => ReportMessage::Written {
            path: target,
            bytes: written,
        },
        Err(ca_report::ReportError::Cancelled) => ReportMessage::Cancelled,
        Err(error) => ReportMessage::Failed {
            reason: format!("{} could not be written: {error}", target.display()),
        },
    }
}

/// A writer that counts what passed through it and reports as it goes.
struct Counting<'a, W: Write> {
    inner: W,
    written: u64,
    reported: u64,
    progress: &'a dyn Fn(u64),
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(buffer)?;
        self.written = self.written.saturating_add(count as u64);
        if self.written.saturating_sub(self.reported) >= PROGRESS_STRIDE {
            self.reported = self.written;
            (self.progress)(self.written);
        }
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::ReportMessage;
    use crate::worker::Terminal;
    use std::path::PathBuf;

    #[test]
    fn only_progress_leaves_the_stream_open() {
        assert!(!ReportMessage::Progress { bytes: 1 }.is_terminal());
        assert!(ReportMessage::Cancelled.is_terminal());
        assert!(ReportMessage::Written {
            path: PathBuf::new(),
            bytes: 0
        }
        .is_terminal());
    }

    #[test]
    fn overlapping_reports_do_not_share_or_remove_each_others_temporary() {
        use crate::report::{
            Payload, ReportKind, ReportMeta, ReportPlan, ReportSettings, Target, TextPayload,
        };
        use crate::worker::Cancel;
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.html");
        let sibling = dir
            .path()
            .join(format!(".out.html.{}.report-part", std::process::id()));
        std::fs::write(&sibling, b"unrelated user data").unwrap();
        let make_plan = |title: &str| {
            let mut settings = ReportSettings::new(ReportKind::Text);
            settings.target = Target::File;
            settings.path = target.display().to_string();
            ReportPlan {
                settings,
                meta: ReportMeta::new("left", "right").with_title(title),
                payload: Payload::Text(TextPayload::default()),
                bytes_per_row: 0,
            }
        };
        let first = make_plan(&"first".repeat(20_000));
        let second = make_plan("second");
        let ran = AtomicBool::new(false);
        let result = super::to_file(&first, &Cancel::new(), &|_| {
            if !ran.swap(true, Ordering::Relaxed) {
                assert!(matches!(
                    super::to_file(&second, &Cancel::new(), &|_| {}),
                    ReportMessage::Written { .. }
                ));
            }
        });
        assert!(ran.load(Ordering::Relaxed));
        assert!(matches!(result, ReportMessage::Written { .. }));
        let mut expected = Vec::new();
        first.write(&mut expected, &Cancel::new()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), expected);
        assert_eq!(std::fs::read(&sibling).unwrap(), b"unrelated user data");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn a_report_sharing_violation_preserves_the_previous_output() {
        use crate::report::{
            Payload, ReportKind, ReportMeta, ReportPlan, ReportSettings, Target, TextPayload,
        };
        use crate::worker::Cancel;
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.html");
        std::fs::write(&target, b"previous report").unwrap();
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&target)
            .unwrap();
        let mut settings = ReportSettings::new(ReportKind::Text);
        settings.target = Target::File;
        settings.path = target.display().to_string();
        let plan = ReportPlan {
            settings,
            meta: ReportMeta::new("left", "right"),
            payload: Payload::Text(TextPayload::default()),
            bytes_per_row: 0,
        };
        assert!(matches!(
            super::to_file(&plan, &Cancel::new(), &|_| {}),
            ReportMessage::Failed { .. }
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"previous report");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
