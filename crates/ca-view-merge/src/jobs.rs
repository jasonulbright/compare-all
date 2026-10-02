//! The background work behind a merge: reading the inputs and merging them.
//!
//! Reading, decoding and merging run on one worker thread, which posts one
//! finished result for the view to apply. Every step polls the cancellation
//! flag, so a newer request stops an older run rather than waiting for it.

use crate::model::{split, Inputs, MergeModel, Pane};
use crate::settings::EngineOptions;
use ca_text::{DecodeOptions, EolStyle, LoadedText};
use ca_ui::save::{Baseline, FileSystem, RealFileSystem, Stamp};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The paths one merge session reads and writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergePaths {
    /// One changed version.
    pub left: PathBuf,
    /// The common ancestor, when there is one.
    pub center: Option<PathBuf>,
    /// The other changed version.
    pub right: PathBuf,
    /// Where the result is written.
    pub output: Option<PathBuf>,
}

impl MergePaths {
    /// The path of one input pane.
    #[must_use]
    pub fn input(&self, pane: Pane) -> Option<&Path> {
        match pane {
            Pane::Left => Some(&self.left),
            Pane::Right => Some(&self.right),
            Pane::Center => self.center.as_deref(),
            Pane::Output => self.output.as_deref(),
        }
    }

    /// True when no ancestor was supplied.
    #[must_use]
    pub const fn is_two_way(&self) -> bool {
        self.center.is_none()
    }
}

/// What one input holds once it is read.
#[derive(Debug, Clone, Default)]
pub struct SideFacts {
    /// Label of the encoding the bytes were decoded with.
    pub encoding: String,
    /// Label of the dominant line ending style.
    pub eol: String,
    /// True when the file holds more than one line ending style.
    pub mixed_eol: bool,
    /// Size in bytes as read.
    pub size: u64,
    /// What the file looked like on disk when it was read.
    pub stamp: Option<Stamp>,
}

/// The files behind a merge, as they were read.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The left file.
    pub left: LoadedText,
    /// The ancestor, when there is one.
    pub center: Option<LoadedText>,
    /// The right file.
    pub right: LoadedText,
    /// Facts about the left file.
    pub left_facts: SideFacts,
    /// Facts about the ancestor.
    pub center_facts: SideFacts,
    /// Facts about the right file.
    pub right_facts: SideFacts,
}

impl Sources {
    /// The facts of one input pane.
    #[must_use]
    pub fn facts(&self, pane: Pane) -> &SideFacts {
        match pane {
            Pane::Left => &self.left_facts,
            Pane::Center => &self.center_facts,
            _ => &self.right_facts,
        }
    }
}

/// A finished merge.
#[derive(Debug, Clone)]
pub struct MergeData {
    /// The merge and its section states.
    pub model: MergeModel,
    /// The files the merge read.
    pub sources: Box<Sources>,
    /// Widest line of any pane, in characters, which is how far the panes
    /// scroll sideways.
    pub widest: u32,
    /// What the output path held when the run started. The view keeps the
    /// first one it is given, so a file written under that name later is a
    /// change and not the state the session started from.
    pub output_baseline: Baseline,
}

/// What the worker posts back.
#[derive(Debug)]
pub enum MergeMessage {
    /// A step of the pipeline has begun.
    Progress(&'static str),
    /// The merge could not be produced.
    Failed(String),
    /// The run stopped before producing a merge.
    Cancelled,
    /// The merge is ready.
    Ready(Box<MergeData>),
}

impl Terminal for MergeMessage {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            MergeMessage::Failed(_) | MergeMessage::Cancelled | MergeMessage::Ready(_)
        )
    }

    fn cancelled() -> Self {
        MergeMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        MergeMessage::Failed(detail)
    }
}

/// How each input's bytes are decoded.
///
/// The ancestor takes the left side's encoding, because the stored settings
/// name one encoding per side and a merge reads three files.
#[derive(Debug, Clone, Default)]
pub struct MergeDecoding {
    /// Decoding of the left input, which the ancestor also uses.
    pub left: DecodeOptions,
    /// Decoding of the right input.
    pub right: DecodeOptions,
}

/// Read the inputs and merge them on a worker thread.
#[must_use]
pub fn spawn(
    paths: MergePaths,
    options: EngineOptions,
    decoding: MergeDecoding,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<MergeMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| run(&paths, &options, &decoding, emitter, cancel),
        notify,
    )
}

fn run(
    paths: &MergePaths,
    options: &EngineOptions,
    decoding: &MergeDecoding,
    emitter: &Emitter<MergeMessage>,
    cancel: &Cancel,
) {
    let progress = |step: &'static str| {
        emitter.send(MergeMessage::Progress(step));
    };
    match read_and_merge(paths, options, decoding, cancel, &progress) {
        Ok(Some(data)) => {
            emitter.send(MergeMessage::Ready(Box::new(data)));
        }
        Ok(None) => {}
        Err(message) => {
            emitter.send(MergeMessage::Failed(message));
        }
    }
}

/// Read the inputs, merge them and classify every section.
///
/// Returns `None` when `cancel` stopped the run. Every step reads files, so this
/// runs on a worker or in a process that has no window.
///
/// # Errors
///
/// Returns the reason when an input cannot be read or the merge fails.
pub fn read_and_merge(
    paths: &MergePaths,
    options: &EngineOptions,
    decoding: &MergeDecoding,
    cancel: &Cancel,
    progress: &dyn Fn(&'static str),
) -> Result<Option<MergeData>, String> {
    progress("Reading files");
    let output_baseline = paths.output.as_deref().map_or(Baseline::Unchecked, |path| {
        Baseline::of(&RealFileSystem, path)
    });
    let left = load(&paths.left, &decoding.left)?;
    let right = load(&paths.right, &decoding.right)?;
    let center = paths
        .center
        .as_deref()
        .map(|path| load(path, &decoding.left))
        .transpose()?;
    if cancel.is_cancelled() {
        return Ok(None);
    }

    progress("Merging");
    let inputs = Inputs {
        left: split(&left.0.buffer.text()),
        center: center
            .as_ref()
            .map(|loaded| split(&loaded.0.buffer.text()))
            .unwrap_or_default(),
        right: split(&right.0.buffer.text()),
        two_way: center.is_none(),
    };
    let widest = widest_line(&inputs);
    let mut model = match MergeModel::build(inputs, &options.merge, cancel) {
        Ok(model) => model,
        Err(ca_diff::DiffError::Cancelled) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if cancel.is_cancelled() {
        return Ok(None);
    }
    model
        .classify(&options.rules)
        .map_err(|error| error.to_string())?;
    let (center_text, center_facts) = match center {
        Some((loaded, facts)) => (Some(loaded), facts),
        None => (None, SideFacts::default()),
    };
    let sources = Box::new(Sources {
        left: left.0,
        center: center_text,
        right: right.0,
        left_facts: left.1,
        center_facts,
        right_facts: right.1,
    });
    Ok(Some(MergeData {
        model,
        sources,
        widest,
        output_baseline,
    }))
}

fn widest_line(inputs: &Inputs) -> u32 {
    [&inputs.left, &inputs.center, &inputs.right]
        .into_iter()
        .flat_map(|lines| lines.iter())
        .map(|line| u32::try_from(line.trim_end_matches(['\n', '\r']).chars().count()).unwrap_or(0))
        .max()
        .unwrap_or(0)
}

fn load(path: &Path, decode: &DecodeOptions) -> Result<(LoadedText, SideFacts), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let stamp = RealFileSystem.stamp(path);
    let loaded = LoadedText::load(&bytes, decode);
    let facts = SideFacts {
        encoding: loaded.spec.encoding.label().to_owned(),
        eol: eol_label(loaded.eol.dominant).to_owned(),
        mixed_eol: loaded.eol.mixed,
        size: bytes.len() as u64,
        stamp,
    };
    Ok((loaded, facts))
}

const fn eol_label(style: EolStyle) -> &'static str {
    match style {
        EolStyle::Lf => "Unix",
        EolStyle::CrLf => "Windows",
        EolStyle::Cr => "Mac",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{spawn, MergeMessage, MergePaths};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn collect(paths: MergePaths) -> Vec<MergeMessage> {
        let mut job = spawn(
            paths,
            crate::settings::options_from(&ca_session::settings::TextMergeSettings::default()),
            super::MergeDecoding::default(),
            Arc::new(|| {}),
        );
        let deadline = Instant::now() + Duration::from_secs(20);
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

    fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text.as_bytes()).unwrap();
        path
    }

    fn ready(messages: Vec<MergeMessage>) -> Box<super::MergeData> {
        messages
            .into_iter()
            .find_map(|message| match message {
                MergeMessage::Ready(data) => Some(data),
                _ => None,
            })
            .expect("the merge finished")
    }

    #[test]
    fn three_files_merge_to_a_model_with_the_files_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", "a\nL\nc\np\nq\nr\ns\n"),
            center: Some(write(dir.path(), "center.txt", "a\nb\nc\np\nq\nr\ns\n")),
            right: write(dir.path(), "right.txt", "a\nb\nc\np\nq\nr\nS\n"),
            output: Some(dir.path().join("out.txt")),
        };
        let data = ready(collect(paths));
        assert_eq!(data.model.output_text(), "a\nL\nc\np\nq\nr\nS\n");
        assert_eq!(data.sources.left_facts.encoding, "UTF-8");
        assert_eq!(data.sources.right_facts.eol, "Unix");
        assert!(data.sources.center.is_some());
        assert!(data.widest >= 1);
    }

    #[test]
    fn two_files_without_an_ancestor_merge_two_way() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", "a\nL\n"),
            center: None,
            right: write(dir.path(), "right.txt", "a\nR\n"),
            output: None,
        };
        let data = ready(collect(paths));
        assert!(data.model.inputs().two_way);
        assert_eq!(data.model.totals().conflicts, 1);
    }

    #[test]
    fn a_missing_input_reports_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", "a\n"),
            center: None,
            right: dir.path().join("absent.txt"),
            output: None,
        };
        assert!(collect(paths)
            .iter()
            .any(|message| matches!(message, MergeMessage::Failed(_))));
    }

    #[test]
    fn a_cancelled_run_posts_no_result() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", "a\n"),
            center: None,
            right: write(dir.path(), "right.txt", "b\n"),
            output: None,
        };
        let mut job = spawn(
            paths,
            crate::settings::options_from(&ca_session::settings::TextMergeSettings::default()),
            super::MergeDecoding::default(),
            Arc::new(|| {}),
        );
        job.cancel();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(job.drain());
            if job.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(job.is_finished());
        assert!(seen
            .iter()
            .all(|message| !matches!(message, MergeMessage::Ready(_))));
    }
}
