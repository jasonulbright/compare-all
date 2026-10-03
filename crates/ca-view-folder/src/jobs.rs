//! The background work behind a folder comparison: scanning both sides,
//! aligning them, running the listing tests, and later streaming the results of
//! the content tests.

use crate::settings::EngineOptions;
use crate::tree::{Arena, Sort};
use ca_fs::{
    align_trees, apply_filters, compare_quick, compare_source_contents_parallel, scan_source,
    scan_with, ArchiveTypes, CompareOptions, ContentMethod, ContentUpdate, FilterContext, Limits,
    NameFilters, Node, RulesEngine, ScanOptions, ScanProgress, Source,
};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// How many entries a scan counts between progress reports.
const PROGRESS_STRIDE: usize = 2_000;

/// Which side a progress report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The left base folder.
    Left,
    /// The right base folder.
    Right,
}

/// What the scan worker posts back.
#[derive(Debug)]
pub enum ScanMessage {
    /// A side is being read.
    Counted {
        /// Which side.
        side: Side,
        /// Entries captured so far.
        entries: usize,
    },
    /// A step of the pipeline has begun.
    Progress(&'static str),
    /// The top level alone, posted while the subfolders are still being read.
    Partial(Box<Arena>),
    /// The scan could not run.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
    /// The comparison is ready, with any per entry errors that were recorded.
    Ready {
        /// The flattened comparison.
        arena: Box<Arena>,
        /// The aligned tree, kept for the content tests.
        tree: Box<Node>,
        /// Entries that could not be read, with what went wrong.
        errors: Vec<ScanFailure>,
        /// True when either root's own listing was not enumerated in full.
        root_incomplete: bool,
        /// What each side was opened as: a local folder or a container read
        /// as one.
        sources: Box<Sides>,
    },
}

/// The two sides of a comparison, as the scan opened them.
#[derive(Debug, Clone)]
pub struct Sides {
    /// The left side.
    pub left: Source,
    /// The right side.
    pub right: Source,
}

impl Sides {
    /// True when both sides are local folders.
    #[must_use]
    pub fn are_local(&self) -> bool {
        self.left.is_local_folder() && self.right.is_local_folder()
    }
}

/// Open one side: a folder, or a container file read as one when the
/// handling allows it.
///
/// A path that is neither a folder nor a file stays a local folder, so the
/// scan reports that its root cannot be read.
///
/// # Errors
/// Returns the message to show when a file cannot be opened as a folder.
pub fn open_side(path: &Path, archives: &ArchiveTypes) -> Result<Source, String> {
    if !path.is_file() {
        return Ok(Source::local(path));
    }
    Source::open(path, archives, &Limits::default()).map_err(|error| error.to_string())
}

/// One entry a scan could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFailure {
    /// Path relative to the base folder, as the scan reported it.
    pub path: PathBuf,
    /// What the file system said.
    pub message: String,
    /// True when the failure is on the right side.
    pub right: bool,
}

impl Terminal for ScanMessage {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            ScanMessage::Failed(_) | ScanMessage::Cancelled | ScanMessage::Ready { .. }
        )
    }

    fn cancelled() -> Self {
        ScanMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        ScanMessage::Failed(detail)
    }
}

/// What the content worker posts back.
#[derive(Debug)]
pub enum ContentMessage {
    /// One pair finished.
    Update(Box<ContentUpdate>),
    /// Every pair finished, or the run stopped early.
    ///
    /// The tree the run was given comes back with it, which is why a cancelled
    /// run has to be drained rather than dropped: dropping the job loses the
    /// comparison until a full rescan rebuilds it.
    Done {
        /// The tree the run mutated, where the worker still held it.
        tree: Option<Box<Node>>,
        /// False when the run stopped before finishing.
        complete: bool,
    },
    /// The run could not proceed.
    Failed(String),
}

impl Terminal for ContentMessage {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            ContentMessage::Done { .. } | ContentMessage::Failed(_)
        )
    }

    fn cancelled() -> Self {
        ContentMessage::Done {
            tree: None,
            complete: false,
        }
    }

    fn panicked(detail: String) -> Self {
        ContentMessage::Failed(detail)
    }
}

/// What the sort worker posts back.
#[derive(Debug)]
pub enum SortMessage {
    /// The reordered comparison.
    Done(Box<Arena>),
    /// The sort stopped before finishing.
    Cancelled,
    /// The sort could not run.
    Failed(String),
}

impl Terminal for SortMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        SortMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        SortMessage::Failed(detail)
    }
}

/// Reorder a comparison on a worker.
///
/// A pass over every node, so a large tree is reordered off the frame thread
/// and the view keeps painting the old order until the new one lands.
#[must_use]
pub fn spawn_sort(
    arena: Box<Arena>,
    sort: Sort,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<SortMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let mut arena = arena;
            arena.sort(sort);
            if cancel.is_cancelled() {
                emitter.send(SortMessage::Cancelled);
                return;
            }
            emitter.send(SortMessage::Done(arena));
        },
        notify,
    )
}

/// Scan, align and quick compare two folders on a worker thread.
///
/// `name_filter` is the one line form the toolbar and the exclude command edit;
/// the four mask lists of `options` are applied alongside it.
pub fn spawn_scan(
    left: PathBuf,
    right: PathBuf,
    name_filter: String,
    options: Box<EngineOptions>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<ScanMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| run_scan(&left, &right, &name_filter, &options, emitter, cancel),
        notify,
    )
}

/// The masks of a session: the four stored lists and the one line form.
#[must_use]
pub fn name_filters_of(options: &EngineOptions, name_filter: &str) -> NameFilters {
    let mut filters = NameFilters::from_lists(
        &options.include_files.join(";"),
        &options.exclude_files.join(";"),
        &options.include_folders.join(";"),
        &options.exclude_folders.join(";"),
    );
    let extra = NameFilters::parse(name_filter);
    filters.include.extend(extra.include);
    filters.exclude.extend(extra.exclude);
    filters.case = options.alignment.case;
    filters
}

fn run_scan(
    left: &Path,
    right: &Path,
    name_filter: &str,
    options: &EngineOptions,
    emitter: &Emitter<ScanMessage>,
    cancel: &Cancel,
) {
    emitter.send(ScanMessage::Progress("Scanning"));
    let sides = match (
        open_side(left, &options.archives),
        open_side(right, &options.archives),
    ) {
        (Ok(left), Ok(right)) => Sides { left, right },
        (Err(message), _) | (_, Err(message)) => {
            emitter.send(ScanMessage::Failed(message));
            return;
        }
    };
    // A container is listed in one walk, so only two local folders gain from
    // a top level pass ahead of the full one.
    if options.handling.background_subfolders && sides.are_local() {
        // The top level alone answers first, so the tree is on screen while the
        // rest of the walk runs. A failure here is reported by the full pass.
        let shallow = ScanOptions {
            max_depth: Some(1),
            ..options.scan.clone()
        };
        if let (Ok(left_top), Ok(right_top)) = (
            scan_with(left, &shallow, cancel.as_fs(), &|_| {}),
            scan_with(right, &shallow, cancel.as_fs(), &|_| {}),
        ) {
            if !cancel.is_cancelled() {
                let mut top =
                    align_trees(&left_top, &right_top, &options.alignment, cancel.as_fs());
                compare_quick(&mut top, &options.compare);
                emitter.send(ScanMessage::Partial(Box::new(Arena::from_root(&top))));
            }
        }
    }
    let left_scan = match scan_side(&sides.left, Side::Left, &options.scan, emitter, cancel) {
        Ok(result) => result,
        Err(message) => {
            emitter.send(ScanMessage::Failed(message));
            return;
        }
    };
    let right_scan = match scan_side(&sides.right, Side::Right, &options.scan, emitter, cancel) {
        Ok(result) => result,
        Err(message) => {
            emitter.send(ScanMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(ScanMessage::Progress("Aligning"));
    let mut tree = align_trees(&left_scan, &right_scan, &options.alignment, cancel.as_fs());
    if cancel.is_cancelled() {
        return;
    }
    let names = name_filters_of(options, name_filter);
    let mut others = options.other_filters.clone();
    others.left_root = left.to_path_buf();
    others.right_root = right.to_path_buf();
    apply_filters(&mut tree, &names, &others, &FilterContext::default());
    emitter.send(ScanMessage::Progress("Comparing"));
    compare_quick(&mut tree, &options.compare);
    if cancel.is_cancelled() {
        return;
    }
    let arena = Arena::from_root(&tree);
    let mut errors: Vec<ScanFailure> = left_scan
        .errors
        .iter()
        .map(|error| ScanFailure {
            path: error.rel.clone(),
            message: error.message.clone(),
            right: false,
        })
        .collect();
    errors.extend(right_scan.errors.iter().map(|error| ScanFailure {
        path: error.rel.clone(),
        message: error.message.clone(),
        right: true,
    }));
    emitter.send(ScanMessage::Ready {
        arena: Box::new(arena),
        tree: Box::new(tree),
        errors,
        root_incomplete: left_scan.root_incomplete || right_scan.root_incomplete,
        sources: Box::new(sides),
    });
}

fn scan_side(
    source: &Source,
    side: Side,
    options: &ScanOptions,
    emitter: &Emitter<ScanMessage>,
    cancel: &Cancel,
) -> Result<ca_fs::ScanResult, String> {
    let counter = AtomicUsize::new(0);
    let report = |progress: ScanProgress<'_>| {
        if matches!(progress, ScanProgress::Entry(_)) {
            let seen = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if seen.is_multiple_of(PROGRESS_STRIDE) {
                emitter.send(ScanMessage::Counted {
                    side,
                    entries: seen,
                });
            }
        }
    };
    let result = scan_source(source, options, cancel.as_fs(), &report)
        .map_err(|error| format!("{}: {error}", source.label()))?;
    emitter.send(ScanMessage::Counted {
        side,
        entries: result.entries.len(),
    });
    Ok(result)
}

/// Run the content tests over an already aligned tree, streaming each result.
///
/// The tree is moved onto the worker for the duration and handed back when the
/// run ends, so no copy of a large comparison is made and the view never reads
/// a tree a worker is writing.
pub fn spawn_contents(
    tree: Box<Node>,
    sides: Sides,
    method: ContentMethod,
    engine: Box<EngineOptions>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<ContentMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let mut tree = tree;
            let mut options = CompareOptions {
                quick: engine.compare.quick.clone(),
                content: engine.compare.content.clone(),
            };
            options.content.enabled = true;
            options.content.method = method;
            options.content.skip_if_quick_same = false;
            let rules = (method == ContentMethod::Rules).then(|| {
                RulesEngine::with_builtin_formats().with_format_lists(
                    engine.enabled_formats.clone(),
                    engine.disabled_formats.clone(),
                )
            });
            let complete = compare_source_contents_parallel(
                &mut tree,
                &sides.left,
                &sides.right,
                &options,
                rules
                    .as_ref()
                    .map(|engine| engine as &dyn ca_fs::RulesComparer),
                cancel.as_fs(),
                &|update| {
                    emitter.send(ContentMessage::Update(Box::new(update.clone())));
                },
            );
            emitter.send(ContentMessage::Done {
                tree: Some(tree),
                complete,
            });
        },
        notify,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{spawn_contents, spawn_scan, ContentMessage, ScanMessage};
    use ca_fs::ContentMethod;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn drain<M: ca_ui::worker::Terminal>(job: &mut ca_ui::worker::Job<M>) -> Vec<M> {
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

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        std::fs::create_dir_all(left.join("sub")).unwrap();
        std::fs::create_dir_all(right.join("sub")).unwrap();
        std::fs::write(left.join("same.txt"), b"same").unwrap();
        std::fs::write(right.join("same.txt"), b"same").unwrap();
        std::fs::write(left.join("sub/differs.txt"), b"one").unwrap();
        std::fs::write(right.join("sub/differs.txt"), b"two").unwrap();
        std::fs::write(left.join("orphan.txt"), b"only here").unwrap();
        dir
    }

    /// Options with the two-phase scan off, so a test sees one result only.
    fn options() -> crate::settings::EngineOptions {
        let mut options =
            crate::settings::options_of(&ca_session::settings::FolderCompareSettings::default());
        options.handling.background_subfolders = false;
        options
    }

    fn scan(dir: &Path, filter: &str) -> (Box<crate::tree::Arena>, Box<ca_fs::Node>) {
        scan_with_options(dir, filter, options())
    }

    fn scan_with_options(
        dir: &Path,
        filter: &str,
        options: crate::settings::EngineOptions,
    ) -> (Box<crate::tree::Arena>, Box<ca_fs::Node>) {
        let mut job = spawn_scan(
            dir.join("left"),
            dir.join("right"),
            filter.to_string(),
            Box::new(options),
            Arc::new(|| {}),
        );
        drain(&mut job)
            .into_iter()
            .find_map(|message| match message {
                ScanMessage::Ready { arena, tree, .. } => Some((arena, tree)),
                _ => None,
            })
            .expect("the scan finished")
    }

    #[test]
    fn a_scan_produces_an_aligned_arena() {
        let dir = fixture();
        let (arena, _) = scan(dir.path(), "");
        assert_eq!(arena.len(), 4);
        let totals = arena.totals();
        assert_eq!(totals.folders, 1);
        assert_eq!(totals.files, 3);
        assert_eq!(totals.orphans, 1);
    }

    #[test]
    fn a_name_filter_narrows_the_comparison() {
        let dir = fixture();
        let (arena, _) = scan(dir.path(), "same.txt");
        let names: Vec<&str> = arena
            .nodes()
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert!(names.contains(&"same.txt"));
        assert!(!names.contains(&"orphan.txt"));
    }

    #[test]
    fn a_missing_root_reports_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = spawn_scan(
            dir.path().join("absent-left"),
            dir.path().join("absent-right"),
            String::new(),
            Box::new(options()),
            Arc::new(|| {}),
        );
        assert!(drain(&mut job)
            .iter()
            .any(|message| matches!(message, ScanMessage::Failed(_))));
    }

    #[test]
    fn content_results_stream_and_the_tree_comes_back() {
        let dir = fixture();
        let (mut arena, tree) = scan(dir.path(), "");
        let mut job = spawn_contents(
            tree,
            super::Sides {
                left: ca_fs::Source::local(dir.path().join("left")),
                right: ca_fs::Source::local(dir.path().join("right")),
            },
            ContentMethod::Binary,
            Box::new(options()),
            Arc::new(|| {}),
        );
        let messages = drain(&mut job);
        let mut updates = 0;
        let mut finished = false;
        for message in messages {
            match message {
                ContentMessage::Update(update) => {
                    arena.apply_content(&update, false);
                    updates += 1;
                }
                ContentMessage::Done { complete, .. } => finished = complete,
                ContentMessage::Failed(reason) => panic!("the content run failed: {reason}"),
            }
        }
        arena.roll_up();
        assert!(updates >= 2, "only {updates} pairs were compared");
        assert!(finished);
        let differs = arena
            .index_of(Path::new("sub").join("differs.txt").as_path())
            .expect("the differing pair is in the tree");
        assert_eq!(
            arena.node(differs).unwrap().status,
            ca_fs::NodeStatus::Different
        );
    }

    /// The 22 bytes of an end of central directory record: a zip holding
    /// nothing.
    const EMPTY_ZIP: &[u8] = &[
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    /// A left folder and a right zip that hold the same names as the fixture,
    /// with `sub/differs.txt` changed.
    fn folder_and_zip() -> tempfile::TempDir {
        let dir = fixture();
        let zip = dir.path().join("right.zip");
        std::fs::write(&zip, EMPTY_ZIP).unwrap();
        let source = ca_fs::Source::archive(&zip, ca_vfs::ArchiveOptions::default()).unwrap();
        for (name, bytes) in [
            ("same.txt", b"same".as_slice()),
            ("sub/differs.txt", b"two"),
        ] {
            let mut reader = bytes;
            source
                .file_system()
                .write_file(
                    &ca_vfs::VfsPath::parse(name).unwrap(),
                    &mut reader,
                    &ca_vfs::Cancel::new(),
                )
                .unwrap();
        }
        dir
    }

    fn scan_messages(
        left: &Path,
        right: &Path,
        options: crate::settings::EngineOptions,
    ) -> Vec<ScanMessage> {
        let mut job = spawn_scan(
            left.to_path_buf(),
            right.to_path_buf(),
            String::new(),
            Box::new(options),
            Arc::new(|| {}),
        );
        drain(&mut job)
    }

    #[test]
    fn an_archive_side_scans_and_compares_as_a_folder() {
        let dir = folder_and_zip();
        let messages = scan_messages(
            &dir.path().join("left"),
            &dir.path().join("right.zip"),
            options(),
        );
        let (mut arena, tree, sides) = messages
            .into_iter()
            .find_map(|message| match message {
                ScanMessage::Ready {
                    arena,
                    tree,
                    sources,
                    ..
                } => Some((arena, tree, sources)),
                ScanMessage::Failed(reason) => panic!("the scan failed: {reason}"),
                _ => None,
            })
            .expect("the scan finished");
        assert!(sides.left.is_local_folder());
        assert_eq!(sides.right.kind(), ca_fs::SourceKind::Archive);
        let totals = arena.totals();
        assert_eq!(totals.folders, 1);
        assert_eq!(totals.files, 3);
        assert_eq!(totals.orphans, 1);

        let mut job = spawn_contents(
            tree,
            *sides,
            ContentMethod::Binary,
            Box::new(options()),
            Arc::new(|| {}),
        );
        for message in drain(&mut job) {
            match message {
                ContentMessage::Update(update) => {
                    arena.apply_content(&update, false);
                }
                ContentMessage::Done { complete, .. } => assert!(complete),
                ContentMessage::Failed(reason) => panic!("the content run failed: {reason}"),
            }
        }
        arena.roll_up();
        let status = |rel: &Path| arena.node(arena.index_of(rel).unwrap()).unwrap().status;
        assert_eq!(status(Path::new("same.txt")), ca_fs::NodeStatus::Same);
        assert_eq!(
            status(Path::new("sub").join("differs.txt").as_path()),
            ca_fs::NodeStatus::Different
        );
    }

    #[test]
    fn the_as_files_handling_leaves_an_archive_side_unopened() {
        let dir = folder_and_zip();
        let mut settings = ca_session::settings::FolderCompareSettings::default();
        settings.handling.archive_handling = ca_session::settings::folder::ArchiveHandling::AsFiles;
        let mut options = crate::settings::options_of(&settings);
        options.handling.background_subfolders = false;
        let messages = scan_messages(
            &dir.path().join("left"),
            &dir.path().join("right.zip"),
            options,
        );
        let failure = messages
            .iter()
            .find_map(|message| match message {
                ScanMessage::Failed(reason) => Some(reason.clone()),
                _ => None,
            })
            .expect("the scan refused the archive");
        assert!(failure.contains("right.zip"), "{failure}");
    }

    /// A left tree whose top level holds one orphan folder with a file in it.
    fn orphan_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("left/only")).unwrap();
        std::fs::create_dir_all(dir.path().join("right")).unwrap();
        std::fs::write(dir.path().join("left/only/inside.txt"), b"x").unwrap();
        dir
    }

    #[test]
    fn the_top_level_orphan_setting_changes_what_the_scan_produces() {
        let dir = orphan_fixture();
        let (skipped, _) = scan(dir.path(), "");
        assert_eq!(
            skipped.len(),
            1,
            "the stored default reports the folder only"
        );

        let mut options = options();
        options.alignment.scan_top_level_orphans = true;
        let (scanned, _) = scan_with_options(dir.path(), "", options);
        assert_eq!(scanned.len(), 2, "the orphan folder and its file");
    }

    #[test]
    fn the_stored_exclude_masks_narrow_the_scan_without_the_toolbar_line() {
        let dir = fixture();
        let mut options = options();
        options.exclude_files = vec!["orphan.txt".to_owned()];
        let (arena, _) = scan_with_options(dir.path(), "", options);
        let names: Vec<&str> = arena
            .nodes()
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert!(!names.contains(&"orphan.txt"));
        assert!(names.contains(&"same.txt"));
    }

    #[test]
    fn a_size_filter_item_narrows_the_scan() {
        let dir = fixture();
        let mut options = options();
        options.other_filters.items = vec![ca_fs::OtherFilter::LargerThan(5)];
        let (arena, _) = scan_with_options(dir.path(), "", options);
        let names: Vec<&str> = arena
            .nodes()
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert!(
            !names.contains(&"orphan.txt"),
            "nine bytes is over the bound"
        );
        assert!(names.contains(&"same.txt"));
    }

    #[test]
    fn a_content_filter_item_narrows_the_scan() {
        let dir = fixture();
        let mut options = options();
        options.other_filters.content = vec![ca_fs::ContentFilter {
            text: "only here".to_owned(),
            not_containing: false,
        }];
        let (arena, _) = scan_with_options(dir.path(), "", options);
        let names: Vec<&str> = arena
            .nodes()
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert!(!names.contains(&"orphan.txt"));
        assert!(names.contains(&"same.txt"));
    }

    #[test]
    fn an_alignment_override_pairs_two_names_in_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("left")).unwrap();
        std::fs::create_dir_all(dir.path().join("right")).unwrap();
        std::fs::write(dir.path().join("left/app.dev.cfg"), b"x").unwrap();
        std::fs::write(dir.path().join("right/app.live.cfg"), b"x").unwrap();

        let (plain, _) = scan(dir.path(), "");
        assert_eq!(plain.len(), 2);

        let mut options = options();
        options.alignment.overrides = vec![ca_fs::AlignmentOverride {
            left: "app.dev.*".to_owned(),
            right: "app.live.*".to_owned(),
            limit_to_folder: String::new(),
        }];
        let (paired, _) = scan_with_options(dir.path(), "", options);
        assert_eq!(paired.len(), 1);
    }

    #[test]
    fn the_background_setting_posts_the_top_level_before_the_whole_tree() {
        let dir = fixture();
        let mut options = options();
        options.handling.background_subfolders = true;
        let mut job = spawn_scan(
            dir.path().join("left"),
            dir.path().join("right"),
            String::new(),
            Box::new(options),
            Arc::new(|| {}),
        );
        let messages = drain(&mut job);
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, ScanMessage::Partial(_))),
            "no top level answer arrived"
        );
    }

    #[test]
    fn the_rules_method_reports_a_whitespace_difference_as_unimportant() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("left")).unwrap();
        std::fs::create_dir_all(dir.path().join("right")).unwrap();
        std::fs::write(
            dir.path().join("left/a.rs"),
            b"let a = 1;
",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("right/a.rs"),
            b"let    a = 1;
",
        )
        .unwrap();
        let (_, tree) = scan(dir.path(), "");
        let mut job = spawn_contents(
            tree,
            super::Sides {
                left: ca_fs::Source::local(dir.path().join("left")),
                right: ca_fs::Source::local(dir.path().join("right")),
            },
            ContentMethod::Rules,
            Box::new(options()),
            Arc::new(|| {}),
        );
        let outcome = drain(&mut job)
            .into_iter()
            .find_map(|message| match message {
                ContentMessage::Update(update) => update.outcome.clone().ok(),
                _ => None,
            });
        assert!(
            matches!(
                outcome,
                Some(
                    ca_fs::ContentOutcome::UnimportantDifferences
                        | ca_fs::ContentOutcome::RulesSame
                )
            ),
            "{outcome:?}"
        );
    }
}

/// What the extraction worker posts back.
#[derive(Debug)]
pub enum ExtractMessage {
    /// Both files are on the local disk. The request opens them read-only and
    /// names the copies its tab deletes.
    Ready(Box<ca_ui::view::OpenRequest>),
    /// A copy could not be made. Every copy made so far is already gone.
    Failed(String),
    /// The run was stopped.
    Cancelled,
}

impl Terminal for ExtractMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        ExtractMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        ExtractMessage::Failed(detail)
    }
}

/// Copy the entry at `rel` out of each side that is not a local folder, into
/// a folder of its own under `directory`.
///
/// A local side keeps its own path. The request that comes back is `request`
/// over the copies, read-only, with each copy folder named for deletion.
///
/// # Errors
/// Returns the message to show when a side cannot be read or a copy cannot be
/// written. The copies already made are deleted first.
pub fn extract_pair(
    sides: &Sides,
    rel: &Path,
    directory: &Path,
    request: ca_ui::view::OpenRequest,
    cancel: &Cancel,
) -> Result<ca_ui::view::OpenRequest, String> {
    let mut copies = Vec::new();
    let mut paths = Vec::new();
    for source in [&sides.left, &sides.right] {
        if source.is_local_folder() {
            paths.push(source.origin().join(rel));
            continue;
        }
        match extract_one(source, rel, directory, cancel) {
            Ok((folder, file)) => {
                copies.push(folder);
                paths.push(file);
            }
            Err(reason) => {
                for folder in &copies {
                    let _ = std::fs::remove_dir_all(folder);
                }
                return Err(reason);
            }
        }
    }
    let mut request = request;
    let mut paths = paths.into_iter();
    request.left = paths.next().unwrap_or_default();
    request.right = paths.next().unwrap_or_default();
    Ok(request.over_temporaries(copies))
}

/// One entry copied into a new folder under `directory`: the folder and the
/// file inside it.
fn extract_one(
    source: &Source,
    rel: &Path,
    directory: &Path,
    cancel: &Cancel,
) -> Result<(PathBuf, PathBuf), String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let mut path = source.root().clone();
    for part in rel.components() {
        path = path
            .join(&part.as_os_str().to_string_lossy())
            .map_err(|error| format!("{}: {error}", rel.display()))?;
    }
    let name = rel
        .file_name()
        .ok_or_else(|| format!("{} names no file", rel.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let folder = directory.join(format!(
        "{}-{stamp}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = folder.join(name);
    let copy = || -> Result<(), String> {
        std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let vfs_cancel = ca_vfs::Cancel::from_flag(cancel.as_fs().as_flag());
        let mut reader = source
            .file_system()
            .open(&path, &vfs_cancel)
            .map_err(|error| error.to_string())?;
        let mut writer = std::fs::File::create(&file).map_err(|error| error.to_string())?;
        std::io::copy(&mut reader, &mut writer).map_err(|error| error.to_string())?;
        if cancel.is_cancelled() {
            return Err("the copy was stopped".to_owned());
        }
        Ok(())
    };
    match copy() {
        Ok(()) => Ok((folder, file)),
        Err(reason) => {
            let _ = std::fs::remove_dir_all(&folder);
            Err(format!(
                "{} could not be copied out: {reason}",
                rel.display()
            ))
        }
    }
}

/// Start copying the entry at `rel` out of the sides on a worker.
#[must_use]
pub fn spawn_extract(
    sides: Sides,
    rel: PathBuf,
    request: ca_ui::view::OpenRequest,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<ExtractMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let message = match ca_ui::paths::prepared_temporary_directory() {
                Ok(directory) => match extract_pair(&sides, &rel, &directory, request, cancel) {
                    Ok(request) => ExtractMessage::Ready(Box::new(request)),
                    Err(reason) => ExtractMessage::Failed(reason),
                },
                Err(error) => ExtractMessage::Failed(format!(
                    "{} could not be copied out: the folder for copies {} cannot be used: {error}",
                    rel.display(),
                    ca_ui::paths::temporary_directory().display()
                )),
            };
            emitter.send(message);
        },
        notify,
    )
}
