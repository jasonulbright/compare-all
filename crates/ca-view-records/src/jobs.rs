//! The background work behind a record comparison.
//!
//! Reading the two sides, parsing them into record trees, comparing the trees
//! and flattening the result into display rows all run on one worker thread.
//! The frame thread only paints what the worker posted.
//!
//! [`spawn_load`] reads both sides. [`spawn_recompare`] starts from trees the
//! view already holds, so a settings change does not read the sides again.
//! [`spawn_rebuild`] applies a plan of registry edits to the models as read
//! and compares the result, so an edit shows in the panes without a write.

use crate::flavor::Flavor;
use crate::model::{flatten, Node};
use crate::session_options::{Rules, MEDIA_DURATION, MEDIA_STREAM_GROUP};
use ca_records::media::{self, MediaCompareOptions, MediaFormat, MediaReadOptions};
use ca_records::registry::plan::{DeleteForm, EditOp, EditPlan};
use ca_records::registry::{
    self, live, LiveOptions, RegFile, RegistryCompareOptions, RegistrySpec,
};
use ca_records::version::{self, VersionCompareOptions, VersionReadOptions};
use ca_records::{Importance, Limits, RecordTree, RecordValue, Status, TreeDiff};
use ca_ui::save::{FileSystem, RealFileSystem, Stamp};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Name of the root of a live side's tree. The root is not a row.
const LIVE_ROOT: &str = "Registry";

/// Prefix that marks a side as a live registry key rather than a file.
pub const LIVE_PREFIX: &str = "reg:";

/// One step of the pipeline, in the order it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Reading and parsing the two sides.
    Reading,
    /// Aligning and comparing the two trees.
    Comparing,
}

impl Stage {
    /// What the status bar says while the step runs.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reading => "Reading",
            Self::Comparing => "Comparing",
        }
    }
}

/// What reading one side found out about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SideFacts {
    /// What the side was read as.
    pub format: String,
    /// Size of the file in bytes, absent for a live registry key.
    pub size: Option<u64>,
    /// Last modification time of the file, where the file system states one.
    pub modified: Option<SystemTime>,
    /// What the file looked like when it was read, for the changed-on-disk
    /// check of a save. Absent for a live registry key.
    pub stamp: Option<Stamp>,
}

/// One side of a comparison, as read.
#[derive(Debug, Clone)]
pub struct Side {
    /// The records the side holds.
    pub tree: Arc<RecordTree>,
    /// What reading the side found out.
    pub facts: SideFacts,
    /// The registry model the tree was built from: the export file, or an
    /// export of the live key. Absent for the other flavors.
    pub model: Option<Arc<RegFile>>,
    /// The base key of a live registry side.
    pub live: Option<RegistrySpec>,
}

impl Side {
    /// The same side over another registry model.
    #[must_use]
    fn with_model(&self, model: RegFile) -> Self {
        Self {
            tree: Arc::new(model.to_tree(&self.tree.name)),
            facts: self.facts.clone(),
            model: Some(Arc::new(model)),
            live: self.live.clone(),
        }
    }
}

/// A finished comparison.
#[derive(Debug, Clone)]
pub struct RecordData {
    /// Left side.
    pub left: Side,
    /// Right side.
    pub right: Side,
    /// The merged tree, flattened into display rows.
    pub nodes: Arc<Vec<Node>>,
    /// True when the sides were read from their sources by this run, false
    /// when the run compared sides the view already held.
    pub fresh: bool,
}

/// What the worker posts back.
#[derive(Debug)]
pub enum RecordMessage {
    /// A step of the pipeline has begun.
    Progress(Stage),
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
    /// The comparison is ready.
    Ready(Box<RecordData>),
    /// The plan of edits could not be applied to the models; the sides on
    /// screen stay as they were.
    Refused(String),
}

impl Terminal for RecordMessage {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Failed(_) | Self::Cancelled | Self::Ready(_) | Self::Refused(_)
        )
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// Read both sides and compare them on a worker thread.
#[must_use]
pub fn spawn_load(
    flavor: Flavor,
    left: PathBuf,
    right: PathBuf,
    rules: Rules,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<RecordMessage> {
    spawn_load_with_clipboard(flavor, left, right, rules, [None, None], notify)
}

/// Read the sides, using captured registry export text where supplied.
///
/// Clipboard contents stay in memory; no file is created for them.
#[must_use]
pub fn spawn_load_with_clipboard(
    flavor: Flavor,
    left: PathBuf,
    right: PathBuf,
    rules: Rules,
    clipboard: [Option<String>; 2],
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<RecordMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            load_and_compare(flavor, &left, &right, &rules, &clipboard, emitter, cancel);
        },
        notify,
    )
}

/// Compare two sides the caller already holds.
#[must_use]
pub fn spawn_recompare(
    flavor: Flavor,
    left: Side,
    right: Side,
    rules: Rules,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<RecordMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| compare_sides(flavor, left, right, &rules, false, emitter, cancel),
        notify,
    )
}

/// Apply `ops` to the registry models of two sides as read, then compare.
///
/// The models change in memory only. An operation that does not fit the
/// models posts [`RecordMessage::Refused`] with the reason.
#[must_use]
pub fn spawn_rebuild(
    left: Side,
    right: Side,
    ops: Vec<EditOp>,
    rules: Rules,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<RecordMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            emitter.send(RecordMessage::Progress(Stage::Comparing));
            match edited_sides(&left, &right, ops) {
                Ok((left, right)) => {
                    compare_sides(
                        Flavor::Registry,
                        left,
                        right,
                        &rules,
                        false,
                        emitter,
                        cancel,
                    );
                }
                Err(reason) => {
                    emitter.send(RecordMessage::Refused(reason));
                }
            }
        },
        notify,
    )
}

/// The two sides with `ops` applied to their registry models.
///
/// # Errors
///
/// Returns the reason when a side has no model or an operation does not fit.
pub fn edited_sides(left: &Side, right: &Side, ops: Vec<EditOp>) -> Result<(Side, Side), String> {
    if ops.is_empty() {
        return Ok((left.clone(), right.clone()));
    }
    let (Some(left_model), Some(right_model)) = (left.model.as_ref(), right.model.as_ref()) else {
        return Err("only a registry comparison can be edited".to_owned());
    };
    let mut left_model = RegFile::clone(left_model);
    let mut right_model = RegFile::clone(right_model);
    EditPlan {
        ops,
        ..EditPlan::new()
    }
    .apply_to_files_with(&mut left_model, &mut right_model, DeleteForm::Remove)
    .map_err(|error| error.to_string())?;
    Ok((left.with_model(left_model), right.with_model(right_model)))
}

fn load_and_compare(
    flavor: Flavor,
    left: &Path,
    right: &Path,
    rules: &Rules,
    clipboard: &[Option<String>; 2],
    emitter: &Emitter<RecordMessage>,
    cancel: &Cancel,
) {
    emitter.send(RecordMessage::Progress(Stage::Reading));
    let left_side = match read_input(flavor, left, clipboard[0].as_deref()) {
        Ok(side) => side,
        Err(message) => {
            emitter.send(RecordMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }
    let right_side = match read_input(flavor, right, clipboard[1].as_deref()) {
        Ok(side) => side,
        Err(message) => {
            emitter.send(RecordMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }
    compare_sides(flavor, left_side, right_side, rules, true, emitter, cancel);
}

fn read_input(flavor: Flavor, path: &Path, clipboard: Option<&str>) -> Result<Side, String> {
    let Some(text) = clipboard else {
        return read_side(flavor, path);
    };
    if flavor != Flavor::Registry {
        return Err("This comparison requires binary file data, not clipboard text.".to_owned());
    }
    let file = RegFile::parse(text.as_bytes(), &Limits::default())
        .map_err(|error| format!("Clipboard: {error}"))?;
    let tree = file.to_tree("Clipboard");
    Ok(Side {
        tree: Arc::new(tree),
        facts: SideFacts {
            format: "Registry clipboard text".to_owned(),
            size: u64::try_from(text.len()).ok(),
            modified: None,
            stamp: None,
        },
        model: Some(Arc::new(file)),
        live: None,
    })
}

fn compare_sides(
    flavor: Flavor,
    left: Side,
    right: Side,
    rules: &Rules,
    fresh: bool,
    emitter: &Emitter<RecordMessage>,
    cancel: &Cancel,
) {
    emitter.send(RecordMessage::Progress(Stage::Comparing));
    let diff = compare(flavor, &left.tree, &right.tree, rules);
    if cancel.is_cancelled() {
        return;
    }
    let nodes = flatten(diff);
    if cancel.is_cancelled() {
        return;
    }
    emitter.send(RecordMessage::Ready(Box::new(RecordData {
        left,
        right,
        nodes: Arc::new(nodes),
        fresh,
    })));
}

/// Compare two trees under `rules`.
#[must_use]
pub fn compare(flavor: Flavor, left: &RecordTree, right: &RecordTree, rules: &Rules) -> TreeDiff {
    let align = rules.align.clone();
    let mut diff = match flavor {
        Flavor::Registry => registry::compare(
            left,
            right,
            &RegistryCompareOptions {
                align,
                ..RegistryCompareOptions::default()
            },
        ),
        Flavor::Version => version::compare(
            left,
            right,
            &VersionCompareOptions {
                align,
                ..VersionCompareOptions::default()
            },
        ),
        Flavor::Media => media::compare(
            left,
            right,
            &MediaCompareOptions {
                align,
                ..MediaCompareOptions::default()
            },
        ),
    };
    apply_rules(&mut diff, rules);
    diff
}

/// Apply the rules the engine has no option for: whole groups set unimportant
/// and the play time tolerance.
///
/// Only record statuses and importances change. A group's rolled up class is
/// worked out again when the tree is flattened, so it follows these changes.
fn apply_rules(diff: &mut TreeDiff, rules: &Rules) {
    if rules.unimportant_groups.is_empty() && rules.duration_tolerance_ms == 0 {
        return;
    }
    let mut stack: Vec<&mut TreeDiff> = vec![diff];
    while let Some(node) = stack.pop() {
        for record in &mut node.records {
            if rules.group_is_unimportant(&record.path) {
                record.importance = Importance::Unimportant;
            }
            if rules.duration_tolerance_ms > 0
                && record.status == Status::Different
                && record.path.eq_ignore_ascii_case(MEDIA_STREAM_GROUP)
                && record.name.eq_ignore_ascii_case(MEDIA_DURATION)
            {
                let within = match (
                    record.left.as_ref().map(|held| &held.value),
                    record.right.as_ref().map(|held| &held.value),
                ) {
                    (Some(RecordValue::Integer(a)), Some(RecordValue::Integer(b))) => {
                        a.abs_diff(*b) <= u128::from(rules.duration_tolerance_ms)
                    }
                    _ => false,
                };
                if within {
                    record.status = Status::Same;
                }
            }
        }
        stack.extend(node.children.iter_mut());
    }
}

/// True when `path` names a live registry key rather than a file.
#[must_use]
pub fn is_live_spec(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.trim_start()
        .get(..LIVE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(LIVE_PREFIX))
}

/// Read one side into a record tree.
///
/// # Errors
///
/// Returns the message the status bar shows when the side cannot be read or
/// parsed.
pub fn read_side(flavor: Flavor, path: &Path) -> Result<Side, String> {
    if flavor == Flavor::Registry && is_live_spec(path) {
        return read_live(path);
    }
    // The stamp is taken before the read, so a write between the two shows
    // as a change at save time rather than being overwritten.
    let stamp = RealFileSystem.stamp(path);
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let metadata = std::fs::metadata(path).ok();
    let failed = |error: ca_records::RecordError| format!("{}: {error}", path.display());
    let mut model = None;
    let (tree, format) = match flavor {
        Flavor::Registry => {
            let file = RegFile::parse(&bytes, &Limits::default()).map_err(failed)?;
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let tree = file.to_tree(&name);
            model = Some(Arc::new(file));
            (tree, "Registry file".to_owned())
        }
        Flavor::Version => {
            let tree = version::read(&bytes, &VersionReadOptions::default()).map_err(failed)?;
            (tree, "Version resource".to_owned())
        }
        Flavor::Media => {
            let format = MediaFormat::detect(&bytes).map_or("Media", MediaFormat::name);
            let tree = media::read(&bytes, &MediaReadOptions::default()).map_err(failed)?;
            (tree, format.to_owned())
        }
    };
    Ok(Side {
        tree: Arc::new(tree),
        facts: SideFacts {
            format,
            size: metadata.as_ref().map(std::fs::Metadata::len),
            modified: metadata.and_then(|held| held.modified().ok()),
            stamp,
        },
        model,
        live: None,
    })
}

/// Read a live registry key.
///
/// The key is read with query and enumerate rights only. A key on another
/// machine is refused by the engine with its reason.
fn read_live(path: &Path) -> Result<Side, String> {
    let text = path.to_string_lossy();
    let spec = RegistrySpec::parse(text.trim()).map_err(|error| error.to_string())?;
    let model = live::export(&spec, &LiveOptions::default())
        .map_err(|error| format!("{}: {error}", spec.to_spec_string()))?;
    Ok(Side {
        tree: Arc::new(live_tree(&model)),
        facts: SideFacts {
            format: "Live registry".to_owned(),
            size: None,
            modified: None,
            stamp: None,
        },
        model: Some(Arc::new(model)),
        live: Some(spec),
    })
}

/// The tree of a live side's model.
///
/// A block path starts at the hive, so the tree hangs the key under the chain
/// of keys above it and aligns against an export file of the same key.
#[must_use]
pub fn live_tree(model: &RegFile) -> RecordTree {
    model.to_tree(LIVE_ROOT)
}

/// The one comparison a view has in flight.
///
/// Starting a new job cancels the one before it, so a stale result never
/// reaches the screen.
#[derive(Default)]
pub struct Pipeline {
    job: Option<Job<RecordMessage>>,
    superseded: u64,
}

impl Pipeline {
    /// A pipeline with nothing running.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take over from whatever was running.
    pub fn start(&mut self, job: Job<RecordMessage>) {
        if let Some(previous) = self.job.take() {
            previous.cancel();
            self.superseded += 1;
        }
        self.job = Some(job);
    }

    /// Take whatever the running job has posted.
    pub fn poll(&mut self) -> Vec<RecordMessage> {
        let Some(job) = self.job.as_mut() else {
            return Vec::new();
        };
        let messages = job.drain();
        if job.is_finished() {
            self.job = None;
        }
        messages
    }

    /// Ask the running job to stop, keeping the handle so its terminal message
    /// still reaches the view.
    pub fn cancel(&self) {
        if let Some(job) = self.job.as_ref() {
            job.cancel();
        }
    }

    /// Drop the running job outright.
    pub fn stop(&mut self) {
        self.job = None;
    }

    /// True while a job is held.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// How many jobs were replaced before they finished.
    #[must_use]
    pub const fn superseded_count(&self) -> u64 {
        self.superseded
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{compare, is_live_spec, live_tree, read_side, spawn_load, RecordMessage};
    use crate::flavor::Flavor;
    use crate::session_options::Rules;
    use crate::testing;
    use ca_records::registry::{RegFile, RegFileVersion, RegKeyBlock, RegistrySpec};
    use ca_records::{Importance, Status};
    use ca_ui::testing::wait_until;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn a_reg_prefix_names_a_live_key() {
        assert!(is_live_spec(Path::new(r"reg:\\HKCU\Software")));
        assert!(is_live_spec(Path::new(r"REG:\\HKEY_USERS")));
        assert!(!is_live_spec(Path::new("export.reg")));
    }

    #[test]
    fn a_live_key_hangs_at_its_full_path() {
        let spec = RegistrySpec::parse(r"reg:\\HKCU\Software\Example").unwrap();
        let mut model = RegFile::empty(RegFileVersion::V5);
        model.keys.push(RegKeyBlock {
            path: spec.key_path(),
            delete: false,
            entries: Vec::new(),
            source: ca_records::ByteRange::default(),
        });
        let root = live_tree(&model);
        let hive = root.child("HKEY_CURRENT_USER").unwrap();
        let software = hive.child("Software").unwrap();
        assert_eq!(software.path, r"HKEY_CURRENT_USER\Software");
        assert!(software.child("Example").is_some());
    }

    #[test]
    fn a_remote_key_is_refused_with_a_reason() {
        let failure = read_side(
            Flavor::Registry,
            Path::new(r"reg:\\SomeMachine\HKEY_USERS\Example"),
        )
        .unwrap_err();
        assert!(failure.contains("remote") || failure.contains("machine"));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_local_live_key_is_refused_with_the_platform_reason() {
        let failure = read_side(
            Flavor::Registry,
            Path::new(r"reg:\\HKEY_CURRENT_USER\Software"),
        )
        .unwrap_err();
        assert!(failure.contains("no Windows registry"), "{failure}");
    }

    #[test]
    fn two_export_files_load_and_compare_on_a_worker() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut job = spawn_load(
            Flavor::Registry,
            left,
            right,
            Rules::default(),
            Arc::new(|| {}),
        );
        let mut seen = Vec::new();
        assert!(wait_until(Duration::from_secs(30), || {
            seen.extend(job.drain());
            job.is_finished()
        }));
        let data = seen
            .into_iter()
            .find_map(|message| match message {
                RecordMessage::Ready(data) => Some(data),
                _ => None,
            })
            .expect("the comparison finished");
        assert!(data.nodes.iter().any(|node| node.name == "Changed"));
        assert_eq!(data.left.facts.format, "Registry file");
    }

    #[test]
    fn a_group_set_unimportant_marks_every_record_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::version_pair(dir.path());
        let left = read_side(Flavor::Version, &left).unwrap();
        let right = read_side(Flavor::Version, &right).unwrap();
        let rules = Rules {
            unimportant_groups: vec!["StringFileInfo".to_owned()],
            ..Rules::default()
        };
        let diff = compare(Flavor::Version, &left.tree, &right.tree, &rules);
        let mut seen = 0;
        let mut stack = vec![&diff];
        while let Some(node) = stack.pop() {
            for record in &node.records {
                if record.path.starts_with("StringFileInfo") {
                    seen += 1;
                    assert_eq!(record.importance, Importance::Unimportant);
                }
            }
            stack.extend(node.children.iter());
        }
        assert!(seen > 0);
    }

    #[test]
    fn two_play_times_inside_the_tolerance_compare_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join("left.mp3");
        let right_path = dir.path().join("right.mp3");
        std::fs::write(&left_path, testing::mp3(&[("TIT2", "Song")], 40)).unwrap();
        std::fs::write(&right_path, testing::mp3(&[("TIT2", "Song")], 41)).unwrap();
        let left = read_side(Flavor::Media, &left_path).unwrap();
        let right = read_side(Flavor::Media, &right_path).unwrap();
        let strict = compare(Flavor::Media, &left.tree, &right.tree, &Rules::default());
        let loose = compare(
            Flavor::Media,
            &left.tree,
            &right.tree,
            &Rules {
                duration_tolerance_ms: 1_000,
                ..Rules::default()
            },
        );
        let duration = |diff: &ca_records::TreeDiff| {
            diff.children
                .iter()
                .find(|group| group.name == "Audio")
                .and_then(|group| group.records.iter().find(|held| held.name == "Duration"))
                .map(|held| held.status)
        };
        assert_eq!(duration(&strict), Some(Status::Different));
        assert_eq!(duration(&loose), Some(Status::Same));
    }

    #[test]
    fn the_engine_group_names_the_rules_rely_on_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.mp3");
        std::fs::write(&path, testing::mp3(&[("TIT2", "Song")], 4)).unwrap();
        let side = read_side(Flavor::Media, &path).unwrap();
        assert!(side
            .tree
            .child(crate::session_options::MEDIA_STREAM_GROUP)
            .is_some());
        assert!(side.tree.child("ID3v2").is_some());
        assert_eq!(side.facts.format, "MPEG audio");
        let (left, _) = testing::version_pair(dir.path());
        let side = read_side(Flavor::Version, &left).unwrap();
        assert!(side
            .tree
            .child(crate::session_options::VERSION_STRING_GROUP)
            .is_some());
    }

    #[test]
    fn the_pipeline_stops_running_in_the_poll_that_delivers_the_terminal_message() {
        let mut pipeline = super::Pipeline::new();
        let (job, _held) = ca_ui::testing::job_held_after(vec![
            RecordMessage::Progress(super::Stage::Comparing),
            RecordMessage::Refused("the edit does not fit".to_owned()),
        ]);
        pipeline.start(job);
        let messages = pipeline.poll();
        assert!(matches!(
            messages.as_slice(),
            [RecordMessage::Progress(_), RecordMessage::Refused(_)]
        ));
        assert!(
            !pipeline.is_running(),
            "the pipeline still runs a job that has answered"
        );
    }
}
