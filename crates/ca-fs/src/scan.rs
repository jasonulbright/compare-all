//! Cancellable folder tree scanning with per-entry error collection.

use std::collections::{BTreeMap, HashSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cancel::Cancel;
use crate::source::{EntryFacts, Source};

/// File system attributes captured for one entry.
///
/// The four boolean flags are the ones the folder view renders as letters. On
/// Unix they are derived: `hidden` from a leading period, the rest stay false
/// unless the platform reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attributes {
    /// Write access is denied by the file system flag or by the owner write bit.
    pub read_only: bool,
    /// Hidden flag on Windows, leading period on Unix.
    pub hidden: bool,
    /// System flag; always false off Windows.
    pub system: bool,
    /// Archive flag; always false off Windows.
    pub archive: bool,
    /// Raw Windows attribute word when the platform supplies one.
    pub windows_bits: Option<u32>,
    /// Raw Unix mode word when the platform supplies one.
    pub unix_mode: Option<u32>,
    /// Unix owner id.
    pub uid: Option<u32>,
    /// Unix group id.
    pub gid: Option<u32>,
}

impl Attributes {
    /// The permission bits of [`Attributes::unix_mode`], without the file type
    /// bits.
    #[must_use]
    pub fn permission_bits(&self) -> Option<u32> {
        self.unix_mode.map(|mode| mode & 0o7777)
    }

    /// True when the platform marks the data as not resident, so reading it
    /// would trigger a download from a cloud provider.
    #[must_use]
    pub fn is_cloud_placeholder(&self) -> bool {
        const OFFLINE: u32 = 0x0000_1000;
        const RECALL_ON_OPEN: u32 = 0x0004_0000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        self.windows_bits
            .is_some_and(|bits| bits & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0)
    }
}

/// How an entry is linked into the tree.
///
/// Junctions and directory symbolic links both report
/// [`LinkKind::DirectoryLink`]; telling them apart requires reading the reparse
/// tag, which this scanner does not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkKind {
    /// A link whose target is a file, or whose target kind is unknown.
    FileLink,
    /// A directory reparse point: a junction or a directory symbolic link.
    DirectoryLink,
}

/// Metadata captured for one file system entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the scan root, using the platform separator.
    pub rel: PathBuf,
    /// Final path component.
    pub name: String,
    /// True for directories and for directory links, whether or not links are
    /// followed. Refusing to descend into a link is a separate decision.
    pub is_dir: bool,
    /// Size in bytes; zero for directories.
    pub size: u64,
    /// Last modification time when the platform reports one.
    pub modified: Option<SystemTime>,
    /// Creation time when the platform reports one.
    pub created: Option<SystemTime>,
    /// Captured attributes.
    pub attributes: Attributes,
    /// Set when the entry itself is a link, whether or not links are followed.
    pub link: Option<LinkKind>,
    /// Set on a directory whose children were not enumerated in full, so the
    /// absence of a name under it proves nothing. A file is never incomplete.
    pub listing_incomplete: bool,
    /// Text of the error that stopped the entry being read in full. The entry
    /// still carries whatever the listing supplied.
    pub error: Option<String>,
    /// Set when the source lists the entry but refuses to open or extract it,
    /// so no test can rule on a pair that holds it. `error` says why.
    #[serde(default)]
    pub refused: bool,
}

impl Entry {
    /// True when the entry is a symbolic link, junction or other reparse point.
    #[must_use]
    pub fn is_link(&self) -> bool {
        self.link.is_some()
    }
}

/// One entry that could not be read, recorded so the scan can continue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryError {
    /// Path relative to the scan root.
    pub rel: PathBuf,
    /// Text of the underlying I/O error.
    pub message: String,
}

/// Knobs for a scan.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Resolve links and report the target's kind, size, times and attributes.
    /// With this off a link is a leaf and is never descended into.
    pub follow_links: bool,
    /// Maximum depth below the root; `None` is unlimited, `Some(1)` reads only
    /// the root's own children.
    pub max_depth: Option<usize>,
    /// Upper bound on directories read at once. `None` uses the shared pool,
    /// `Some(1)` reads one directory at a time, which suits a network share
    /// that degrades under concurrent requests.
    pub max_threads: Option<usize>,
}

/// Progress reported while a scan runs.
///
/// Events arrive from several worker threads unless `max_threads` is one.
#[derive(Debug)]
pub enum ScanProgress<'a> {
    /// A directory is about to be read.
    EnteringDirectory(&'a Path),
    /// An entry has been captured.
    Entry(&'a Entry),
    /// An entry could not be read and was recorded instead.
    Error(&'a EntryError),
}

/// Outcome of a scan.
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    /// Captured entries keyed by relative path.
    pub entries: BTreeMap<PathBuf, Entry>,
    /// Entries that could not be read.
    pub errors: Vec<EntryError>,
    /// True when the scan stopped early because the cancel flag was set.
    pub cancelled: bool,
    /// True when the root's own listing was not enumerated in full.
    pub root_incomplete: bool,
    /// What each entry's size and time stamp prove, for the entries whose
    /// source states less than a local listing does.
    ///
    /// A path with no record here carries [`EntryFacts::default`], which is
    /// what a local folder always produces.
    pub facts: BTreeMap<PathBuf, EntryFacts>,
}

impl ScanResult {
    /// What one entry's size and time stamp prove.
    #[must_use]
    pub fn facts_of(&self, rel: &Path) -> EntryFacts {
        self.facts.get(rel).copied().unwrap_or_default()
    }
}

/// Scan one side of a comparison, whatever it is stored in.
///
/// A local folder takes the path scanner, so the results and the cost are the
/// same as [`scan_with`] on the same tree. Every other source is walked
/// through the file system trait, and the facts a listing states about its own
/// precision land in [`ScanResult::facts`].
///
/// # Errors
/// Returns [`ScanError::Io`] when a local root cannot be read and
/// [`ScanError::Source`] when a source refuses its own root listing.
pub fn scan_source(
    source: &Source,
    options: &ScanOptions,
    cancel: &Cancel,
    progress: &(dyn Fn(ScanProgress<'_>) + Sync),
) -> Result<ScanResult, ScanError> {
    if source.is_local_folder() {
        return scan_with(source.origin(), options, cancel, progress);
    }
    let vfs_cancel = ca_vfs::Cancel::from_flag(cancel.as_flag());
    let listed = match ca_vfs::walk(source.file_system().as_ref(), source.root(), &vfs_cancel) {
        Ok(listed) => listed,
        Err(ca_vfs::VfsError::Cancelled) => {
            return Ok(ScanResult {
                cancelled: true,
                root_incomplete: true,
                ..ScanResult::default()
            })
        }
        Err(error) => return Err(ScanError::Source(error.to_string())),
    };

    let mut result = ScanResult::default();
    for listed_entry in listed {
        if cancel.is_cancelled() {
            result.cancelled = true;
            result.root_incomplete = true;
            return Ok(result);
        }
        let rel = native_path(&listed_entry.path);
        if rel.as_os_str().is_empty() {
            continue;
        }
        let facts = EntryFacts::of(&listed_entry);
        let entry = convert(&rel, &listed_entry, options);
        if let Some(message) = entry.error.clone() {
            result.errors.push(EntryError {
                rel: rel.clone(),
                message,
            });
        }
        progress(ScanProgress::Entry(&entry));
        if facts != EntryFacts::default() {
            result.facts.insert(rel.clone(), facts);
        }
        result.entries.insert(rel, entry);
    }
    Ok(result)
}

/// Scan one selected entry and, when it is a folder, its descendants.
///
/// Paths in the returned result remain relative to `source`'s root, and the
/// selected entry itself is included. A missing entry returns `Ok(None)`;
/// other read failures are errors so a caller can keep its current tree.
///
/// # Errors
/// Returns [`ScanError::Source`] when the source cannot resolve or list the
/// selected entry, and [`ScanError::Io`] when a local parent cannot be read.
pub fn scan_source_subtree(
    source: &Source,
    rel: &Path,
    options: &ScanOptions,
    cancel: &Cancel,
) -> Result<Option<ScanResult>, ScanError> {
    if rel.as_os_str().is_empty() {
        return Err(ScanError::Source(
            "the source root is not a selectable item".to_owned(),
        ));
    }
    if cancel.is_cancelled() {
        return Ok(Some(ScanResult {
            cancelled: true,
            root_incomplete: true,
            ..ScanResult::default()
        }));
    }
    let path = source
        .entry_path(rel)
        .map_err(|error| ScanError::Source(error.to_string()))?;

    if source.is_local_folder() {
        return scan_local_subtree(source, rel, options, cancel);
    }

    let listed = match source.file_system().metadata(&path) {
        Ok(listed) => listed,
        Err(ca_vfs::VfsError::NotFound { .. }) => return Ok(None),
        Err(error) => return Err(ScanError::Source(error.to_string())),
    };
    let root_rel = native_path(&listed.path);
    let mut result = ScanResult::default();
    insert_source_entry(&mut result, &root_rel, &listed, options);

    if listed.is_dir() {
        match ca_vfs::walk(
            source.file_system().as_ref(),
            &path,
            &ca_vfs::Cancel::from_flag(cancel.as_flag()),
        ) {
            Ok(entries) => {
                for listed in entries {
                    let entry_rel = native_path(&listed.path);
                    insert_source_entry(&mut result, &entry_rel, &listed, options);
                }
            }
            Err(ca_vfs::VfsError::Cancelled) => {
                result.cancelled = true;
                result.root_incomplete = true;
            }
            Err(ca_vfs::VfsError::NotFound { .. } | ca_vfs::VfsError::NotADirectory { .. }) => {
                return Ok(None);
            }
            Err(error) => return Err(ScanError::Source(error.to_string())),
        }
    }
    Ok(Some(result))
}

fn scan_local_subtree(
    source: &Source,
    rel: &Path,
    options: &ScanOptions,
    cancel: &Cancel,
) -> Result<Option<ScanResult>, ScanError> {
    let parent_rel = rel.parent().unwrap_or_else(|| Path::new(""));
    let parent_path = source.origin().join(parent_rel);
    let Some(mut parent_scan) = scan_local_parent(&parent_path, options, cancel)? else {
        return Ok(None);
    };
    if parent_scan.cancelled {
        return Ok(Some(ScanResult {
            cancelled: true,
            root_incomplete: true,
            ..ScanResult::default()
        }));
    }
    let parent_name = local_final_name(rel)?;
    let Some(mut root_entry) = take_local_selection(&mut parent_scan, &parent_name)? else {
        return Ok(None);
    };

    let mut result = ScanResult::default();
    if let Some(facts) = parent_scan.facts.remove(&parent_name) {
        if facts != EntryFacts::default() {
            result.facts.insert(rel.to_path_buf(), facts);
        }
    }
    for error in parent_scan.errors {
        if error.rel == parent_name {
            result.errors.push(EntryError {
                rel: rel.to_path_buf(),
                message: error.message,
            });
        }
    }
    root_entry.rel = rel.to_path_buf();
    root_entry.listing_incomplete = false;
    if root_entry.is_dir {
        let still_present =
            scan_local_descendants(source, rel, options, cancel, &mut root_entry, &mut result)?;
        if !still_present {
            return Ok(None);
        }
    }

    add_entry_error(&mut result, rel, &root_entry);
    result.entries.insert(rel.to_path_buf(), root_entry);
    Ok(Some(result))
}

fn scan_local_parent(
    parent_path: &Path,
    options: &ScanOptions,
    cancel: &Cancel,
) -> Result<Option<ScanResult>, ScanError> {
    match std::fs::metadata(parent_path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(None),
        Err(error) if is_missing_path(&error) => return Ok(None),
        Err(error) => return Err(ScanError::Io(error)),
    }
    let shallow = ScanOptions {
        max_depth: Some(1),
        ..options.clone()
    };
    match scan_with(parent_path, &shallow, cancel, &|_| {}) {
        Ok(scan) => Ok(Some(scan)),
        Err(ScanError::Io(error)) if is_missing_path(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_missing_path(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

fn local_final_name(rel: &Path) -> Result<PathBuf, ScanError> {
    rel.file_name().map(PathBuf::from).ok_or_else(|| {
        ScanError::Source(format!(
            "the selected path has no final component: {}",
            rel.display()
        ))
    })
}

fn take_local_selection(
    parent_scan: &mut ScanResult,
    parent_name: &Path,
) -> Result<Option<Entry>, ScanError> {
    if let Some(entry) = parent_scan.entries.remove(parent_name) {
        return Ok(Some(entry));
    }
    if let Some(error) = parent_scan
        .errors
        .iter()
        .find(|error| error.rel == parent_name)
    {
        return Err(ScanError::Source(error.message.clone()));
    }
    if parent_scan.root_incomplete {
        let reason = parent_scan.errors.first().map_or_else(
            || "the parent listing is incomplete".to_owned(),
            |error| error.message.clone(),
        );
        return Err(ScanError::Source(reason));
    }
    Ok(None)
}

fn scan_local_descendants(
    source: &Source,
    rel: &Path,
    options: &ScanOptions,
    cancel: &Cancel,
    root_entry: &mut Entry,
    result: &mut ScanResult,
) -> Result<bool, ScanError> {
    let depth = rel.components().count();
    let at_depth_limit = options.max_depth.is_some_and(|limit| depth >= limit);
    if root_entry.refused || (root_entry.is_link() && !options.follow_links) || at_depth_limit {
        root_entry.listing_incomplete = true;
        return Ok(true);
    }
    let mut subtree_options = options.clone();
    subtree_options.max_depth = options.max_depth.map(|limit| limit - depth);
    let root_path = source.origin().join(rel);
    let subtree = match scan_with(&root_path, &subtree_options, cancel, &|_| {}) {
        Ok(scan) => scan,
        Err(ScanError::Io(error)) if is_missing_path(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    root_entry.listing_incomplete = subtree.root_incomplete;
    result.root_incomplete = subtree.root_incomplete;
    prefix_result(result, subtree, rel);
    Ok(true)
}

fn add_entry_error(result: &mut ScanResult, rel: &Path, entry: &Entry) {
    if let Some(message) = entry.error.as_ref().filter(|message| {
        !result
            .errors
            .iter()
            .any(|error| error.rel == rel && error.message == **message)
    }) {
        result.errors.push(EntryError {
            rel: rel.to_path_buf(),
            message: message.clone(),
        });
    }
}

fn insert_source_entry(
    result: &mut ScanResult,
    rel: &Path,
    listed: &ca_vfs::VfsEntry,
    options: &ScanOptions,
) {
    let entry = convert(rel, listed, options);
    if let Some(message) = entry.error.clone() {
        result.errors.push(EntryError {
            rel: rel.to_path_buf(),
            message,
        });
    }
    let facts = EntryFacts::of(listed);
    if facts != EntryFacts::default() {
        result.facts.insert(rel.to_path_buf(), facts);
    }
    result.entries.insert(rel.to_path_buf(), entry);
}

fn prefix_result(target: &mut ScanResult, mut source: ScanResult, prefix: &Path) {
    for (rel, mut entry) in source.entries {
        let rel = prefix.join(rel);
        entry.rel.clone_from(&rel);
        target.entries.insert(rel, entry);
    }
    for (rel, facts) in source.facts {
        target.facts.insert(prefix.join(rel), facts);
    }
    target
        .errors
        .extend(source.errors.drain(..).map(|error| EntryError {
            rel: prefix.join(error.rel),
            message: error.message,
        }));
    target.cancelled = source.cancelled;
}

/// The native relative path one virtual path names.
fn native_path(path: &ca_vfs::VfsPath) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        out.push(part);
    }
    out
}

/// The scan entry of one listed entry.
///
/// Every reader places its own stamps on the UTC line: a container places a DOS
/// stamp in the zone it is opened with, and a transfer listing places a clock
/// by the zone its profile names. The stamps are taken as they are listed.
fn convert(rel: &Path, listed: &ca_vfs::VfsEntry, options: &ScanOptions) -> Entry {
    let is_dir = listed.is_dir();
    let link = listed.link.map(|kind| match kind {
        ca_vfs::VfsLinkKind::DirectoryLink => LinkKind::DirectoryLink,
        ca_vfs::VfsLinkKind::FileLink => LinkKind::FileLink,
    });
    let attributes = listed
        .attributes
        .map(|source| Attributes {
            read_only: source.read_only,
            hidden: source.hidden,
            system: source.system,
            archive: source.archive,
            windows_bits: source.windows_bits,
            unix_mode: source.unix_mode,
            uid: source.uid,
            gid: source.gid,
        })
        .unwrap_or_default();
    let depth_cut = options
        .max_depth
        .is_some_and(|max_depth| rel.components().count() >= max_depth);
    Entry {
        rel: rel.to_path_buf(),
        name: listed.name.clone(),
        is_dir,
        size: if is_dir { 0 } else { listed.size },
        modified: listed.modified,
        created: listed.created,
        attributes,
        link,
        listing_incomplete: is_dir && (listed.error.is_some() || depth_cut),
        error: listed.error.clone(),
        refused: listed.refused,
    }
}

/// Errors raised while scanning.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Root path does not exist or is not readable.
    #[error("cannot read root: {0}")]
    Io(#[from] std::io::Error),
    /// A source refused its own root listing.
    #[error("cannot read source: {0}")]
    Source(String),
}

/// Recursively scan `root` and return entries keyed by relative path.
///
/// Entries that fail to stat are skipped; one unreadable child does not abort
/// the scan.
///
/// # Errors
/// Returns [`ScanError::Io`] when `root` itself cannot be read.
pub fn scan(root: &Path) -> Result<BTreeMap<PathBuf, Entry>, ScanError> {
    let result = scan_with(root, &ScanOptions::default(), &Cancel::new(), &|_| {})?;
    Ok(result.entries)
}

/// Scan `root`, reporting progress and honouring `cancel`.
///
/// Unreadable children land in [`ScanResult::errors`] rather than ending the
/// scan. Directories are read concurrently; the entry map is keyed by relative
/// path, so the result does not depend on the order the workers finish.
///
/// A link is descended into only when `options.follow_links` is set and its
/// target is not an ancestor of the branch the link sits in, so a link cycle
/// terminates while an alias of a directory reached by another route still
/// reports that directory's contents.
///
/// # Errors
/// Returns [`ScanError::Io`] when `root` itself cannot be read.
pub fn scan_with(
    root: &Path,
    options: &ScanOptions,
    cancel: &Cancel,
    progress: &(dyn Fn(ScanProgress<'_>) + Sync),
) -> Result<ScanResult, ScanError> {
    std::fs::metadata(root)?;

    let mut result = ScanResult::default();
    let branch = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut level = vec![DirTask {
        path: root.to_path_buf(),
        depth: 0,
        branch,
    }];
    let mut incomplete: Vec<PathBuf> = Vec::new();

    while !level.is_empty() {
        if cancel.is_cancelled() {
            result.cancelled = true;
            mark_unread(&mut incomplete, &level, root);
            apply_incompleteness(&mut result, incomplete);
            return Ok(result);
        }
        let outputs = read_level(&level, root, options, cancel, progress);
        let mut next: Vec<DirTask> = Vec::new();
        for output in outputs {
            for (rel, entry) in output.entries {
                result.entries.insert(rel, entry);
            }
            result.errors.extend(output.errors);
            incomplete.extend(output.incomplete);
            next.extend(output.children);
        }
        if cancel.is_cancelled() {
            result.cancelled = true;
            mark_unread(&mut incomplete, &next, root);
            apply_incompleteness(&mut result, incomplete);
            return Ok(result);
        }
        level = next;
    }

    apply_incompleteness(&mut result, incomplete);
    Ok(result)
}

/// A directory that was queued and never read has no listing at all.
fn mark_unread(incomplete: &mut Vec<PathBuf>, pending: &[DirTask], root: &Path) {
    for task in pending {
        incomplete.push(relative(root, &task.path));
    }
}

/// Fold the set of directories whose listing is partial back into the entries
/// they belong to, so a later phase reads incompleteness off the tree.
fn apply_incompleteness(result: &mut ScanResult, incomplete: Vec<PathBuf>) {
    if result.cancelled {
        result.root_incomplete = true;
    }
    for rel in incomplete {
        if rel.as_os_str().is_empty() {
            result.root_incomplete = true;
        } else if let Some(entry) = result.entries.get_mut(&rel) {
            entry.listing_incomplete = true;
        } else {
            // The directory the partial listing belongs to is not itself in the
            // map, so the shortfall can only be reported against the whole run.
            result.root_incomplete = true;
        }
    }
}

struct DirTask {
    path: PathBuf,
    depth: usize,
    branch: PathBuf,
}

struct DirOutput {
    entries: Vec<(PathBuf, Entry)>,
    errors: Vec<EntryError>,
    children: Vec<DirTask>,
    incomplete: Vec<PathBuf>,
}

fn read_level(
    level: &[DirTask],
    root: &Path,
    options: &ScanOptions,
    cancel: &Cancel,
    progress: &(dyn Fn(ScanProgress<'_>) + Sync),
) -> Vec<DirOutput> {
    let run = || {
        level
            .par_iter()
            .map(|task| read_directory(task, root, options, cancel, progress))
            .collect()
    };
    match options.max_threads {
        Some(1) => level
            .iter()
            .map(|task| read_directory(task, root, options, cancel, progress))
            .collect(),
        Some(threads) => rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_or_else(|_| run(), |pool| pool.install(run)),
        None => run(),
    }
}

fn read_directory(
    task: &DirTask,
    root: &Path,
    options: &ScanOptions,
    cancel: &Cancel,
    progress: &(dyn Fn(ScanProgress<'_>) + Sync),
) -> DirOutput {
    let mut output = DirOutput {
        entries: Vec::new(),
        errors: Vec::new(),
        children: Vec::new(),
        incomplete: Vec::new(),
    };
    let rel_dir = relative(root, &task.path);
    progress(ScanProgress::EnteringDirectory(&task.path));

    let reader = match std::fs::read_dir(&task.path) {
        Ok(reader) => reader,
        Err(err) => {
            output.incomplete.push(rel_dir.clone());
            push_error(&mut output, progress, rel_dir, &err);
            return output;
        }
    };

    let mut held = HeldBack::default();
    for item in reader {
        if cancel.is_cancelled() {
            output.incomplete.push(rel_dir.clone());
            return output;
        }
        let item = match item {
            Ok(item) => item,
            Err(err) => {
                output.incomplete.push(rel_dir.clone());
                push_error(&mut output, progress, rel_dir.clone(), &err);
                continue;
            }
        };
        let Some(item) = held.pass(item) else {
            continue;
        };
        let path = item.path();
        let rel = relative(root, &path);

        let mut link = match item.file_type() {
            Ok(file_type) => link_kind(file_type),
            Err(err) => {
                output.incomplete.push(rel_dir.clone());
                push_error(&mut output, progress, rel, &err);
                continue;
            }
        };

        // The listing describes the link itself, so the kind of a link is only
        // known after the target is stated. The call is made for links alone;
        // a plain entry keeps the metadata the listing already carries.
        let mut failure: Option<String> = None;
        let mut target: Option<Metadata> = None;
        if link.is_some() {
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    if meta.is_dir() {
                        link = Some(LinkKind::DirectoryLink);
                    }
                    target = Some(meta);
                }
                Err(err) => {
                    // A link whose target is missing keeps the kind the listing
                    // reported and stays a leaf.
                    if options.follow_links {
                        failure = Some(err.to_string());
                    }
                }
            }
        }
        let meta = if options.follow_links {
            target.or_else(|| item.metadata().ok())
        } else {
            item.metadata().ok()
        };
        let Some(meta) = meta else {
            let message = failure.unwrap_or_else(|| "cannot read entry".to_owned());
            output.incomplete.push(rel_dir.clone());
            output.errors.push(EntryError {
                rel: rel.clone(),
                message,
            });
            if let Some(error) = output.errors.last() {
                progress(ScanProgress::Error(error));
            }
            continue;
        };

        let mut entry = build_entry(rel.clone(), name_of(&path), &meta, link, failure);
        if entry.is_dir {
            let descended = should_descend(&entry, options, task.depth)
                && match child_branch(&task.branch, &path, &entry) {
                    Some(branch) => {
                        output.children.push(DirTask {
                            path,
                            depth: task.depth + 1,
                            branch,
                        });
                        true
                    }
                    None => false,
                };
            // A directory whose children were not read, and one whose contents
            // the platform holds remotely, both leave the absence of a name
            // under them unproven.
            entry.listing_incomplete = !descended || entry.attributes.is_cloud_placeholder();
        }
        progress(ScanProgress::Entry(&entry));
        output.entries.push((rel, entry));
    }

    push_refused(&mut output, held, &rel_dir, progress);
    output
}

/// The items of one directory a path does not reach, held back until every
/// other name of the directory is known.
#[derive(Default)]
struct HeldBack {
    refused: Vec<(std::fs::DirEntry, String, String)>,
}

impl HeldBack {
    /// `item`, unless a path does not reach it, which holds it back.
    fn pass(&mut self, item: std::fs::DirEntry) -> Option<std::fs::DirEntry> {
        if let Some((display, reason)) = ca_vfs::platform_refusal(&item.file_name()) {
            self.refused.push((item, display, reason));
            return None;
        }
        Some(item)
    }
}

/// The final component of `path` as text.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// List the items of one directory whose names a path does not reach.
///
/// A path built from such a name reaches another item, a device or nothing,
/// so each item is listed from its directory record alone, under a spelling
/// no other item of the directory holds, and no later step is given a path to
/// it.
fn push_refused(
    output: &mut DirOutput,
    held: HeldBack,
    rel_dir: &Path,
    progress: &(dyn Fn(ScanProgress<'_>) + Sync),
) {
    if held.refused.is_empty() {
        return;
    }
    // Every other item of the directory is an entry or an error by now, so
    // their names are the ones a refused row must not take.
    let mut taken: HashSet<String> = output
        .entries
        .iter()
        .map(|(_, entry)| entry.name.to_lowercase())
        .chain(output.errors.iter().filter_map(|error| {
            error
                .rel
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
        }))
        .collect();
    for (item, display, reason) in held.refused {
        let name = ca_vfs::free_spelling(&display, &mut taken);
        let rel = rel_dir.join(&name);
        let entry = refused_entry(rel.clone(), name, &item, reason);
        progress(ScanProgress::Entry(&entry));
        output.errors.push(EntryError {
            rel: rel.clone(),
            message: entry.error.clone().unwrap_or_default(),
        });
        output.entries.push((rel, entry));
    }
}

/// The row of a directory item a path does not reach, taken from the
/// directory record alone.
///
/// The row lists under `name`, which no other item of the folder holds, is
/// never descended into, and carries `reason` as its error.
fn refused_entry(rel: PathBuf, name: String, item: &std::fs::DirEntry, reason: String) -> Entry {
    let link = item.file_type().ok().and_then(link_kind);
    let mut entry = match item.metadata() {
        Ok(meta) => build_entry(rel, name, &meta, link, Some(reason)),
        Err(error) => Entry {
            rel,
            name,
            is_dir: false,
            size: 0,
            modified: None,
            created: None,
            attributes: Attributes::default(),
            link,
            listing_incomplete: false,
            error: Some(format!("{reason}; {error}")),
            refused: false,
        },
    };
    entry.refused = true;
    entry.listing_incomplete = entry.is_dir;
    entry
}

fn should_descend(entry: &Entry, options: &ScanOptions, depth: usize) -> bool {
    entry.is_dir
        && (!entry.is_link() || options.follow_links)
        && options
            .max_depth
            .is_none_or(|max_depth| depth + 1 < max_depth)
}

/// The resolved path a child directory occupies in this branch.
///
/// A plain directory extends the parent's path without touching the file
/// system. A link is resolved, and `None` means the target is an ancestor of
/// this branch, so descending would loop.
fn child_branch(parent: &Path, path: &Path, entry: &Entry) -> Option<PathBuf> {
    if entry.is_link() {
        let target = std::fs::canonicalize(path).ok()?;
        if parent.starts_with(&target) {
            return None;
        }
        Some(target)
    } else {
        Some(parent.join(&entry.name))
    }
}

fn push_error(
    output: &mut DirOutput,
    progress: &(dyn Fn(ScanProgress<'_>) + Sync),
    rel: PathBuf,
    err: &std::io::Error,
) {
    let error = EntryError {
        rel,
        message: err.to_string(),
    };
    progress(ScanProgress::Error(&error));
    output.errors.push(error);
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

#[cfg(windows)]
fn link_kind(file_type: std::fs::FileType) -> Option<LinkKind> {
    use std::os::windows::fs::FileTypeExt;
    if !file_type.is_symlink() {
        return None;
    }
    if file_type.is_symlink_dir() {
        Some(LinkKind::DirectoryLink)
    } else {
        Some(LinkKind::FileLink)
    }
}

#[cfg(not(windows))]
fn link_kind(file_type: std::fs::FileType) -> Option<LinkKind> {
    if file_type.is_symlink() {
        Some(LinkKind::FileLink)
    } else {
        None
    }
}

fn build_entry(
    rel: PathBuf,
    name: String,
    meta: &Metadata,
    link: Option<LinkKind>,
    error: Option<String>,
) -> Entry {
    let is_dir = meta.is_dir() || link == Some(LinkKind::DirectoryLink);
    Entry {
        rel,
        attributes: attributes(meta, &name),
        is_dir,
        size: if is_dir { 0 } else { meta.len() },
        modified: meta.modified().ok(),
        created: meta.created().ok(),
        link,
        error,
        name,
        listing_incomplete: false,
        refused: false,
    }
}

#[cfg(windows)]
fn attributes(meta: &Metadata, _name: &str) -> Attributes {
    use std::os::windows::fs::MetadataExt;
    const READ_ONLY: u32 = 0x0000_0001;
    const HIDDEN: u32 = 0x0000_0002;
    const SYSTEM: u32 = 0x0000_0004;
    const ARCHIVE: u32 = 0x0000_0020;
    let bits = meta.file_attributes();
    Attributes {
        read_only: bits & READ_ONLY != 0,
        hidden: bits & HIDDEN != 0,
        system: bits & SYSTEM != 0,
        archive: bits & ARCHIVE != 0,
        windows_bits: Some(bits),
        unix_mode: None,
        uid: None,
        gid: None,
    }
}

#[cfg(not(windows))]
fn attributes(meta: &Metadata, name: &str) -> Attributes {
    use std::os::unix::fs::MetadataExt;
    let mode = meta.mode();
    Attributes {
        read_only: mode & 0o200 == 0,
        hidden: name.starts_with('.'),
        system: false,
        archive: false,
        windows_bits: None,
        unix_mode: Some(mode),
        uid: Some(meta.uid()),
        gid: Some(meta.gid()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(
    clippy::disallowed_methods,
    reason = "test setup runs a platform tool directly"
)]
mod tests {
    use super::{
        scan, scan_source_subtree, scan_with, Cancel, LinkKind, ScanOptions, ScanProgress,
    };
    use crate::source::Source;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), b"hi").unwrap();
        dir
    }

    /// Create a directory link, reporting false when the platform refuses.
    fn directory_link(link: &Path, target: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|out| out.status.success())
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[test]
    fn scan_lists_nested_files_relative_to_root() {
        let dir = tree();
        let entries = scan(dir.path()).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[Path::new("a.txt")].size, 5);
        assert!(entries[Path::new("sub")].is_dir);
        assert_eq!(entries[&Path::new("sub").join("b.txt")].size, 2);
    }

    #[test]
    fn scan_missing_root_is_an_error() {
        assert!(scan(Path::new("Z:/does/not/exist/compare-all")).is_err());
    }

    #[test]
    fn scan_captures_times_and_attributes() {
        let dir = tree();
        let entries = scan(dir.path()).unwrap();
        let file = &entries[Path::new("a.txt")];
        assert!(file.modified.is_some());
        assert!(!file.attributes.read_only);
        assert!(file.link.is_none());
        assert!(file.error.is_none());
        assert_eq!(file.name, "a.txt");
    }

    #[test]
    fn max_depth_limits_recursion() {
        let dir = tree();
        let options = ScanOptions {
            max_depth: Some(1),
            ..ScanOptions::default()
        };
        let result = scan_with(dir.path(), &options, &Cancel::new(), &|_| {}).unwrap();
        assert_eq!(result.entries.len(), 2);
        assert!(!result.entries.contains_key(&Path::new("sub").join("b.txt")));
    }

    #[test]
    fn progress_reports_every_entry() {
        let dir = tree();
        let seen = AtomicUsize::new(0);
        let result = scan_with(
            dir.path(),
            &ScanOptions::default(),
            &Cancel::new(),
            &|event| {
                if matches!(event, ScanProgress::Entry(_)) {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), result.entries.len());
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let dir = tree();
        let cancel = Cancel::new();
        cancel.cancel();
        let result = scan_with(dir.path(), &ScanOptions::default(), &cancel, &|_| {}).unwrap();
        assert!(result.cancelled);
        assert!(result.entries.is_empty());
    }

    #[test]
    fn cancellation_mid_scan_stops_early() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            std::fs::create_dir(dir.path().join(format!("d{i}"))).unwrap();
            std::fs::write(dir.path().join(format!("d{i}/f.txt")), b"x").unwrap();
        }
        let cancel = Cancel::new();
        let seen = AtomicUsize::new(0);
        let result = scan_with(dir.path(), &ScanOptions::default(), &cancel, &|event| {
            if matches!(event, ScanProgress::Entry(_)) && seen.fetch_add(1, Ordering::SeqCst) == 4 {
                cancel.cancel();
            }
        })
        .unwrap();
        assert!(result.cancelled);
        assert!(result.entries.len() < 100);
    }

    #[test]
    fn unreadable_child_is_collected_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), b"x").unwrap();
        let missing = dir.path().join("gone");
        std::fs::create_dir(&missing).unwrap();
        let result =
            scan_with(dir.path(), &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap();
        assert!(result.entries.contains_key(Path::new("ok.txt")));
        assert!(result.errors.is_empty());
    }

    #[test]
    fn single_threaded_scan_matches_the_parallel_one() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            let sub = dir.path().join(format!("d{i}"));
            std::fs::create_dir(&sub).unwrap();
            for j in 0..10 {
                std::fs::write(sub.join(format!("f{j}.txt")), b"x").unwrap();
            }
        }
        let serial = ScanOptions {
            max_threads: Some(1),
            ..ScanOptions::default()
        };
        let one = scan_with(dir.path(), &serial, &Cancel::new(), &|_| {}).unwrap();
        let many = scan_with(dir.path(), &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap();
        let one_keys: Vec<&PathBuf> = one.entries.keys().collect();
        let many_keys: Vec<&PathBuf> = many.entries.keys().collect();
        assert_eq!(one_keys, many_keys);
        assert_eq!(one.entries.len(), 220);
    }

    #[test]
    fn a_link_alias_does_not_hide_the_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("inside.txt"), b"payload").unwrap();
        // The alias sorts before "real", so a shared visited set would claim
        // the target first and leave the real directory reporting nothing.
        if !directory_link(&dir.path().join("alias"), &real) {
            return;
        }
        let options = ScanOptions {
            follow_links: true,
            ..ScanOptions::default()
        };
        let result = scan_with(dir.path(), &options, &Cancel::new(), &|_| {}).unwrap();
        assert!(result
            .entries
            .contains_key(&Path::new("real").join("inside.txt")));
        assert!(result
            .entries
            .contains_key(&Path::new("alias").join("inside.txt")));
    }

    #[test]
    fn a_link_to_its_own_ancestor_is_not_descended() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join("f.txt"), b"x").unwrap();
        if !directory_link(&inner.join("loop"), dir.path()) {
            return;
        }
        let options = ScanOptions {
            follow_links: true,
            ..ScanOptions::default()
        };
        let result = scan_with(dir.path(), &options, &Cancel::new(), &|_| {}).unwrap();
        assert!(result
            .entries
            .contains_key(&Path::new("inner").join("loop")));
        assert!(!result
            .entries
            .contains_key(&Path::new("inner").join("loop").join("inner")));
    }

    #[test]
    fn an_unfollowed_directory_link_is_still_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("inside.txt"), b"x").unwrap();
        if !directory_link(&dir.path().join("alias"), &real) {
            return;
        }
        let result =
            scan_with(dir.path(), &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap();
        let alias = &result.entries[Path::new("alias")];
        assert!(alias.is_dir);
        assert_eq!(alias.link, Some(LinkKind::DirectoryLink));
        assert!(!result
            .entries
            .contains_key(&Path::new("alias").join("inside.txt")));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_link_with_an_unreadable_target_is_one_entry_carrying_the_error() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("dangling"))
            .unwrap();
        let options = ScanOptions {
            follow_links: true,
            ..ScanOptions::default()
        };
        let result = scan_with(dir.path(), &options, &Cancel::new(), &|_| {}).unwrap();
        let entry = &result.entries[Path::new("dangling")];
        assert!(entry.error.is_some());
        assert!(result.errors.is_empty());
    }

    /// Every reader places its own stamps on the UTC line, so the scan takes
    /// a minute, a day, a DOS and an exact stamp as they are listed.
    #[test]
    fn the_scan_takes_every_stamp_as_it_is_listed() {
        use super::convert;
        use ca_vfs::{TimeFidelity, VfsEntry, VfsPath};
        use std::time::{Duration, UNIX_EPOCH};

        let listed = UNIX_EPOCH + Duration::from_secs(86_400);
        for fidelity in [
            TimeFidelity::MinutePrecision,
            TimeFidelity::DayPrecision,
            TimeFidelity::LocalTwoSecond,
            TimeFidelity::Utc,
        ] {
            let mut entry = VfsEntry::file(VfsPath::parse("a.txt").unwrap(), 1);
            entry.modified = Some(listed);
            entry.created = Some(listed);
            entry.time_fidelity = fidelity;
            let scanned = convert(Path::new("a.txt"), &entry, &ScanOptions::default());
            assert_eq!(scanned.modified, Some(listed), "{fidelity:?}");
            assert_eq!(scanned.created, Some(listed), "{fidelity:?}");
        }
    }

    #[test]
    fn a_selected_file_scan_reads_only_that_entry() {
        let dir = tree();
        let source = Source::local(dir.path());
        let result = scan_source_subtree(
            &source,
            Path::new("a.txt"),
            &ScanOptions::default(),
            &Cancel::new(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[Path::new("a.txt")].size, 5);
        assert!(scan_source_subtree(
            &source,
            Path::new("missing.txt"),
            &ScanOptions::default(),
            &Cancel::new(),
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn a_selected_folder_scan_includes_its_row_and_respects_the_depth_limit() {
        let dir = tree();
        std::fs::create_dir_all(dir.path().join("sub/deeper")).unwrap();
        std::fs::write(dir.path().join("sub/deeper/c.txt"), b"deep").unwrap();
        let source = Source::local(dir.path());
        let result = scan_source_subtree(
            &source,
            Path::new("sub"),
            &ScanOptions {
                max_depth: Some(2),
                ..ScanOptions::default()
            },
            &Cancel::new(),
        )
        .unwrap()
        .unwrap();

        assert!(result.entries.contains_key(Path::new("sub")));
        assert!(result.entries.contains_key(Path::new("sub/b.txt")));
        assert!(result.entries[Path::new("sub/deeper")].listing_incomplete);
        assert!(!result.entries.contains_key(Path::new("sub/deeper/c.txt")));
    }

    #[test]
    fn a_selected_directory_link_is_not_followed_when_the_scan_disallows_it() {
        let dir = tree();
        let target = dir.path().join("sub");
        let link = dir.path().join("alias");
        if !directory_link(&link, &target) {
            return;
        }
        let source = Source::local(dir.path());
        let result = scan_source_subtree(
            &source,
            Path::new("alias"),
            &ScanOptions::default(),
            &Cancel::new(),
        )
        .unwrap()
        .unwrap();

        assert!(result.entries.contains_key(Path::new("alias")));
        assert!(!result.entries.contains_key(Path::new("alias/b.txt")));
    }
}
