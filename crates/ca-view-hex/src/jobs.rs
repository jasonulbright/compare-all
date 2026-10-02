//! The background work behind a byte comparison: reading both files and
//! aligning them.
//!
//! The whole pipeline runs on one worker thread and posts a single finished
//! result, so nothing but painting ever happens on the frame thread.
//!
//! Reading has no memory mapping available to it, so a file is read into one
//! buffer reserved to its size and filled in bounded steps. Peak memory is
//! therefore about the size of the two files, and a pair larger than
//! [`MAX_SIDE_BYTES`] is refused with a message rather than attempted.

use crate::model::RowModel;
use ca_diff::{diff_bytes_cancellable, ByteAlignment, DiffError};
use ca_ui::save::{FileSystem, RealFileSystem, Stamp};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Largest file one side may hold.
///
/// The content is held whole so that editing, searching and saving all address
/// the same bytes. The limit is what keeps that promise from turning into an
/// allocation the machine cannot meet.
pub const MAX_SIDE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Bytes read in one step.
///
/// The step bounds how long a read runs between two cancellation checks; it
/// does not bound the buffer, which is the whole file.
const READ_STEP: usize = 1 << 20;

/// What one side holds once it is read.
#[derive(Debug, Clone, Default)]
pub struct SidePayload {
    /// The file's bytes.
    pub bytes: Arc<Vec<u8>>,
    /// The file's length, which is what the layout addresses.
    ///
    /// This is the same as the length of `bytes` after a completed read. The
    /// two are separate so a layout can be built over a length whose bytes are
    /// not all resident, and a row whose bytes are missing paints as
    /// unavailable rather than as zeros.
    pub len: u64,
    /// What the file looked like on disk when it was read.
    pub stamp: Option<Stamp>,
}

impl SidePayload {
    /// A side over bytes already in memory.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            len: bytes.len() as u64,
            bytes: Arc::new(bytes),
            stamp: None,
        }
    }

    /// A run of bytes, or `None` when the run is not resident.
    #[must_use]
    pub fn slice(&self, start: u64, len: u32) -> Option<&[u8]> {
        let start = usize::try_from(start).ok()?;
        let end = start.checked_add(len as usize)?;
        self.bytes.get(start..end)
    }
}

/// A finished comparison.
#[derive(Debug, Clone, Default)]
pub struct HexData {
    /// Left side content.
    pub left: SidePayload,
    /// Right side content.
    pub right: SidePayload,
    /// Row layout of the two sides.
    pub model: RowModel,
    /// How the two sides were matched up.
    pub alignment: ByteAlignment,
    /// True when the sides were read from their files, so the view takes these
    /// bytes as its content. A comparison over the content in memory leaves it
    /// false, and the view keeps its changes and their history.
    pub fresh: bool,
}

/// What the worker posts back.
#[derive(Debug)]
pub enum HexMessage {
    /// A step of the pipeline has begun.
    Progress(&'static str),
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
    /// The comparison is ready.
    Ready(Box<HexData>),
}

impl Terminal for HexMessage {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            HexMessage::Failed(_) | HexMessage::Cancelled | HexMessage::Ready(_)
        )
    }

    fn cancelled() -> Self {
        HexMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        HexMessage::Failed(detail)
    }
}

/// Everything a comparison needs beyond the two paths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompareSettings {
    /// How the two sides are matched up.
    pub alignment: ByteAlignment,
    /// Bytes each row shows.
    pub bytes_per_row: u32,
}

/// Read and compare two files on a worker thread.
pub fn spawn(
    left: PathBuf,
    right: PathBuf,
    settings: CompareSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<HexMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| run(&left, &right, settings, emitter, cancel),
        notify,
    )
}

/// Compare two sides already in memory on a worker thread.
///
/// This is the path an edit takes: nothing is read from disk, so a comparison
/// that follows a change never observes a file that moved under the buffer.
pub fn spawn_bytes(
    left: SidePayload,
    right: SidePayload,
    settings: CompareSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<HexMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            compare(left, right, settings, false, emitter, cancel);
        },
        notify,
    )
}

fn run(
    left: &Path,
    right: &Path,
    settings: CompareSettings,
    emitter: &Emitter<HexMessage>,
    cancel: &Cancel,
) {
    emitter.send(HexMessage::Progress("Reading files"));
    let left = match load(left, cancel) {
        Ok(Some(side)) => side,
        Ok(None) => return,
        Err(message) => {
            emitter.send(HexMessage::Failed(message));
            return;
        }
    };
    let right = match load(right, cancel) {
        Ok(Some(side)) => side,
        Ok(None) => return,
        Err(message) => {
            emitter.send(HexMessage::Failed(message));
            return;
        }
    };
    compare(left, right, settings, true, emitter, cancel);
}

fn compare(
    left: SidePayload,
    right: SidePayload,
    settings: CompareSettings,
    fresh: bool,
    emitter: &Emitter<HexMessage>,
    cancel: &Cancel,
) {
    emitter.send(HexMessage::Progress("Comparing"));
    let hunks = match diff_bytes_cancellable(&left.bytes, &right.bytes, settings.alignment, cancel)
    {
        Ok(hunks) => hunks,
        Err(DiffError::Cancelled) => return,
        Err(error) => {
            emitter.send(HexMessage::Failed(error.to_string()));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }
    emitter.send(HexMessage::Progress("Laying out"));
    let model = RowModel::build(hunks, settings.bytes_per_row);
    if cancel.is_cancelled() {
        return;
    }
    emitter.send(HexMessage::Ready(Box::new(HexData {
        left,
        right,
        model,
        alignment: settings.alignment,
        fresh,
    })));
}

/// Read one file whole, in bounded steps.
///
/// `Ok(None)` stands for a read the flag stopped, which needs no message of its
/// own because the worker layer closes the stream.
fn load(path: &Path, cancel: &Cancel) -> Result<Option<SidePayload>, String> {
    let stamp = RealFileSystem.stamp(path);
    let size = stamp.map_or(0, |stamp| stamp.size);
    if size > MAX_SIDE_BYTES {
        return Err(format!(
            "{} is {size} bytes. This comparison reads files up to {MAX_SIDE_BYTES} bytes.",
            path.display()
        ));
    }
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let reserve = usize::try_from(size).unwrap_or(0);
    let mut bytes: Vec<u8> = Vec::new();
    bytes
        .try_reserve_exact(reserve)
        .map_err(|_| format!("{}: not enough memory for {size} bytes", path.display()))?;
    let mut step = vec![0u8; READ_STEP];
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let read = file
            .read(&mut step)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        let taken = step.get(..read).unwrap_or_default();
        bytes.extend_from_slice(taken);
    }
    Ok(Some(SidePayload {
        len: bytes.len() as u64,
        bytes: Arc::new(bytes),
        stamp,
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{spawn, CompareSettings, HexData, HexMessage};
    use crate::model::DEFAULT_BYTES_PER_ROW;
    use ca_diff::ByteAlignment;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn settings(alignment: ByteAlignment) -> CompareSettings {
        CompareSettings {
            alignment,
            bytes_per_row: DEFAULT_BYTES_PER_ROW,
        }
    }

    fn collect(left: &Path, right: &Path, alignment: ByteAlignment) -> Vec<HexMessage> {
        let mut job = spawn(
            left.to_path_buf(),
            right.to_path_buf(),
            settings(alignment),
            Arc::new(|| {}),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(job.drain());
            if job.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        seen
    }

    fn ready(messages: Vec<HexMessage>) -> Option<Box<HexData>> {
        messages.into_iter().find_map(|message| match message {
            HexMessage::Ready(data) => Some(data),
            _ => None,
        })
    }

    #[test]
    fn two_files_compare_to_a_row_layout_under_every_alignment() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.bin");
        let right = dir.path().join("right.bin");
        // The look-ahead alignment needs a run of matching bytes on each side
        // of a change before it reports the change alone, so the pair is long
        // enough to give it one.
        let mut content: Vec<u8> = (0..4_096u32)
            .map(|index| u8::try_from(index % 251).unwrap_or(0))
            .collect();
        std::fs::write(&left, &content).unwrap();
        if let Some(slot) = content.get_mut(2_000) {
            *slot ^= 0xFF;
        }
        std::fs::write(&right, &content).unwrap();
        for alignment in [
            ByteAlignment::None,
            ByteAlignment::Fast,
            ByteAlignment::Complete,
        ] {
            let data = ready(collect(&left, &right, alignment)).expect("the comparison finished");
            assert_eq!(data.left.len, 4_096);
            assert_eq!(data.model.counts().changed_bytes, 1, "{alignment:?}");
            assert_eq!(data.model.counts().sections, 1, "{alignment:?}");
            assert_eq!(data.alignment, alignment);
        }
    }

    #[test]
    fn identical_files_report_no_difference() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("a.bin");
        let right = dir.path().join("b.bin");
        std::fs::write(&left, vec![7u8; 4_096]).unwrap();
        std::fs::write(&right, vec![7u8; 4_096]).unwrap();
        let data = ready(collect(&left, &right, ByteAlignment::Complete)).unwrap();
        assert_eq!(data.model.counts().sections, 0);
        assert_eq!(data.model.row_count(), 256);
    }

    /// A file larger than one read step has to arrive whole, which is what the
    /// stepped read is easy to get wrong about.
    #[test]
    fn a_file_larger_than_one_read_step_arrives_whole() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("big-left.bin");
        let right = dir.path().join("big-right.bin");
        let size = 3 * super::READ_STEP + 17;
        let mut content: Vec<u8> = Vec::with_capacity(size);
        for index in 0..size {
            content.push(u8::try_from(index % 251).unwrap_or(0));
        }
        std::fs::write(&left, &content).unwrap();
        let mut other = content.clone();
        if let Some(slot) = other.last_mut() {
            *slot ^= 0xFF;
        }
        std::fs::write(&right, &other).unwrap();
        let data = ready(collect(&left, &right, ByteAlignment::Complete)).unwrap();
        assert_eq!(data.left.len, size as u64);
        assert_eq!(data.left.bytes.len(), size);
        assert_eq!(data.left.bytes.as_slice(), content.as_slice());
        assert_eq!(data.model.counts().changed_bytes, 1);
    }

    #[test]
    fn an_empty_side_still_produces_a_layout() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("full.bin");
        let right = dir.path().join("empty.bin");
        std::fs::write(&left, [1u8, 2, 3]).unwrap();
        std::fs::write(&right, []).unwrap();
        let data = ready(collect(&left, &right, ByteAlignment::Complete)).unwrap();
        assert_eq!(data.model.counts().left_only_bytes, 3);
        assert_eq!(data.model.row_count(), 1);
    }

    #[test]
    fn a_missing_file_reports_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("present.bin");
        std::fs::write(&left, [1u8]).unwrap();
        let messages = collect(
            &left,
            &dir.path().join("absent.bin"),
            ByteAlignment::Complete,
        );
        assert!(messages
            .iter()
            .any(|message| matches!(message, HexMessage::Failed(_))));
    }
}
