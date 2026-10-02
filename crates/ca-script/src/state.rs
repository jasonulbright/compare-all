//! What the commands of a script read and change.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ca_fs::{
    AlignmentOptions, AttributeComparison, AttributeKind, Cancel, CompareOptions, ContentMethod,
    FilterContext, FilterTime, NameFilters, Node, NodeStatus, OtherFilter, OtherFilters,
    ScanOptions,
};

use crate::ast::{
    AttrSet, CompareType, ConfirmMode, ContentCriterion, Criteria, CutoffSpec, CutoffValue,
    FilterClause, PathsArg, SelectKind, SelectMask, SelectResult, SideArg, TimestampCriterion,
    TimezoneCriterion,
};
use crate::clock;
use crate::error::ExecError;
use crate::log::Log;

/// Settings that hold for the whole run.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct SessionOptions {
    /// Stop the run at the first failing command.
    pub stop_on_error: bool,
    /// The answer a confirmation gets.
    pub confirm: ConfirmMode,
    /// Build every plan and run none of them.
    pub dry_run: bool,
    /// Folder the write ahead records are written to.
    pub journal_directory: PathBuf,
    /// Whether a `delete` with no `recyclebin=` argument uses the recycle bin.
    pub recycle_bin: bool,
    /// Refuse every write below the left base folder.
    pub left_read_only: bool,
    /// Refuse every write below the right base folder.
    pub right_read_only: bool,
    /// Seconds the local zone runs ahead of UTC. Timestamps written to the log
    /// and timestamps read from a command use it.
    pub offset_seconds: i64,
    /// Where the profile behind a remote location comes from.
    pub profiles: std::sync::Arc<dyn crate::profiles::ProfileLookup>,
    /// Folder relative paths in a script resolve against.
    pub working_directory: PathBuf,
    /// Which names open as which archive format.
    pub archive_types: ca_fs::ArchiveTypes,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            stop_on_error: false,
            confirm: ConfirmMode::Prompt,
            dry_run: false,
            journal_directory: crate::paths::journal_directory(),
            recycle_bin: false,
            left_read_only: false,
            right_read_only: false,
            offset_seconds: 0,
            profiles: std::sync::Arc::new(crate::profiles::NoProfiles),
            working_directory: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            archive_types: ca_fs::ArchiveTypes::default(),
        }
    }
}

/// Which folders are open.
#[derive(Debug, Clone, Default)]
pub struct Expanded {
    /// True when every folder counts as open.
    pub all: bool,
    /// Folders opened one at a time, as paths relative to the base folders.
    pub paths: BTreeSet<PathBuf>,
}

impl Expanded {
    /// True when the contents of this folder take part in later commands.
    ///
    /// The base folders themselves are always open.
    #[must_use]
    pub fn holds(&self, rel: &Path) -> bool {
        self.all || rel.as_os_str().is_empty() || self.paths.contains(rel)
    }

    /// Close every folder.
    pub fn clear(&mut self) {
        self.all = false;
        self.paths.clear();
    }
}

/// The invisible folder comparison a script drives.
#[derive(Debug)]
pub struct Session {
    /// Settings for the whole run.
    pub options: SessionOptions,
    /// The left base folder.
    pub left: Option<PathBuf>,
    /// The right base folder.
    pub right: Option<PathBuf>,
    /// What the left side is stored in. A folder loaded before sources
    /// existed leaves it empty and is read as a local folder.
    pub left_source: Option<ca_fs::Source>,
    /// What the right side is stored in.
    pub right_source: Option<ca_fs::Source>,
    /// The aligned comparison.
    pub tree: Option<Node>,
    /// Which folders are open.
    pub expanded: Expanded,
    /// The selection, as paths relative to the base folders.
    pub selection: BTreeSet<PathBuf>,
    /// The comparison criteria as the script last set them.
    pub criteria: Criteria,
    /// The name masks, as written.
    pub name_masks: String,
    /// The filters that read something other than the name.
    pub other_filters: OtherFilters,
    /// Where the run writes its log.
    pub log: Log,
    /// The content test the last `compare` command used.
    pub last_compare: Option<CompareType>,
}

impl Session {
    /// A session with no folders loaded.
    #[must_use]
    pub fn new(options: SessionOptions) -> Self {
        Self {
            options,
            left: None,
            right: None,
            left_source: None,
            right_source: None,
            tree: None,
            expanded: Expanded::default(),
            selection: BTreeSet::new(),
            criteria: default_comparison_criteria(),
            name_masks: String::new(),
            other_filters: OtherFilters::default(),
            log: Log::default(),
            last_compare: None,
        }
    }

    /// The two base folders, or the failure naming the command that needs them.
    ///
    /// # Errors
    /// Returns [`ExecError::NoComparison`] when no `load` has run.
    pub fn bases(&self, command: &str) -> Result<(&Path, &Path), ExecError> {
        match (&self.left, &self.right) {
            (Some(left), Some(right)) => Ok((left.as_path(), right.as_path())),
            _ => Err(ExecError::NoComparison {
                command: command.to_string(),
            }),
        }
    }

    /// Refuse a command that writes when either side cannot take a write.
    ///
    /// A folder, a writable container and a writable remote location all pass.
    /// A recorded listing and a read-only container do not. The refusal names
    /// the source, and it happens before the command builds a plan, so nothing
    /// is written and then undone.
    ///
    /// # Errors
    /// Returns [`ExecError::NotSupported`] naming the side that cannot take
    /// the write.
    pub fn require_writable_sides(&self, command: &'static str) -> Result<(), ExecError> {
        for source in [&self.left_source, &self.right_source]
            .into_iter()
            .flatten()
        {
            if let Some(reason) = ca_fs::write_refusal(source) {
                return Err(ExecError::not_supported(command, reason));
            }
        }
        Ok(())
    }

    /// Refuse a write whose resolved target is inside a base locked for this run.
    ///
    /// # Errors
    /// Returns [`ExecError::Refused`] when the target cannot be resolved or is
    /// below a locked base folder.
    pub(crate) fn check_path_write(&self, path: &Path, operation: &str) -> Result<(), ExecError> {
        if !self.options.left_read_only && !self.options.right_read_only {
            return Ok(());
        }
        let target = comparable_path(path).map_err(|error| {
            ExecError::Refused(format!(
                "cannot verify the {operation} target {}: {error}",
                path.display()
            ))
        })?;
        for (locked, base) in [
            (self.options.left_read_only, self.left.as_deref()),
            (self.options.right_read_only, self.right.as_deref()),
        ] {
            let Some(base) = base.filter(|_| locked) else {
                continue;
            };
            let base_key = comparable_path(base).map_err(|error| {
                ExecError::Refused(format!(
                    "cannot verify the locked base {}: {error}",
                    base.display()
                ))
            })?;
            if target.starts_with(&base_key) {
                return Err(ExecError::Refused(format!(
                    "{} is read only for this run, so the {operation} target {} was not written",
                    base.display(),
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// The source one side is read through. A folder loaded before sources
    /// existed is read as a local folder.
    #[must_use]
    pub fn source(&self, side: ca_fs::Side) -> Option<ca_fs::Source> {
        let (base, source) = match side {
            ca_fs::Side::Left => (&self.left, &self.left_source),
            ca_fs::Side::Right => (&self.right, &self.right_source),
        };
        source
            .clone()
            .or_else(|| base.as_ref().map(ca_fs::Source::local))
    }

    /// Every side that is not a plain local folder, each mounted under the
    /// path its entries are addressed by.
    ///
    /// A batch whose paths all fall outside these prefixes reaches the local
    /// disk unchanged.
    #[must_use]
    pub fn mounts(&self) -> Vec<ca_fs::Mount> {
        let mut mounts = Vec::new();
        for (base, source) in [
            (self.left.as_ref(), self.left_source.as_ref()),
            (self.right.as_ref(), self.right_source.as_ref()),
        ] {
            let (Some(base), Some(source)) = (base, source) else {
                continue;
            };
            if source.is_local_folder() {
                continue;
            }
            mounts.push(ca_fs::Mount::new(base.clone(), source.clone()));
        }
        mounts
    }

    /// Resolve a path written in a script against the working folder.
    #[must_use]
    pub fn resolve(&self, path: &str) -> PathBuf {
        let candidate = PathBuf::from(path);
        if candidate.is_absolute() {
            candidate
        } else {
            self.options.working_directory.join(candidate)
        }
    }

    /// Read both base folders and align them.
    ///
    /// # Errors
    /// Returns [`ExecError::Io`] when a base folder cannot be read.
    pub fn rescan(&mut self) -> Result<(), ExecError> {
        let (left, right) = match (&self.left, &self.right) {
            (Some(left), Some(right)) => (left.clone(), right.clone()),
            _ => {
                return Err(ExecError::NoComparison {
                    command: "load".to_string(),
                })
            }
        };
        let cancel = Cancel::default();
        let options = ScanOptions {
            follow_links: self.criteria.follow_symlinks,
            ..ScanOptions::default()
        };
        let left_side = self
            .source(ca_fs::Side::Left)
            .unwrap_or_else(|| ca_fs::Source::local(&left));
        let right_side = self
            .source(ca_fs::Side::Right)
            .unwrap_or_else(|| ca_fs::Source::local(&right));
        let left_scan =
            ca_fs::scan_source(&left_side, &options, &cancel, &|_| {}).map_err(|source| {
                ExecError::Io {
                    context: format!("cannot read {}", left.display()),
                    source: std::io::Error::other(source.to_string()),
                }
            })?;
        let right_scan =
            ca_fs::scan_source(&right_side, &options, &cancel, &|_| {}).map_err(|source| {
                ExecError::Io {
                    context: format!("cannot read {}", right.display()),
                    source: std::io::Error::other(source.to_string()),
                }
            })?;
        let mut tree = ca_fs::align_trees(
            &left_scan,
            &right_scan,
            &AlignmentOptions::default(),
            &cancel,
        );
        self.apply_filters(&mut tree);
        ca_fs::compare_quick(&mut tree, &self.compare_options());
        self.tree = Some(tree);
        self.selection.clear();
        Ok(())
    }

    /// Run the quick tests again over the loaded comparison.
    pub fn recompare(&mut self) {
        let options = self.compare_options();
        if let Some(tree) = self.tree.as_mut() {
            ca_fs::compare_quick(tree, &options);
        }
    }

    fn apply_filters(&self, tree: &mut Node) {
        let names = NameFilters::parse(&self.name_masks);
        let context = FilterContext {
            now: std::time::SystemTime::now(),
            local_offset_seconds: i32::try_from(self.options.offset_seconds).unwrap_or(0),
        };
        ca_fs::apply_filters(tree, &names, &self.other_filters, &context);
    }

    /// The comparison settings the current criteria stand for.
    #[must_use]
    pub fn compare_options(&self) -> CompareOptions {
        let mut options = CompareOptions::default();
        options.quick.size = true;
        options.quick.timestamp = self.criteria.timestamp.is_some();
        if let Some(stamp) = self.criteria.timestamp {
            options.quick.tolerance_seconds = stamp.tolerance_seconds.unwrap_or(0);
            options.quick.ignore_daylight_saving = stamp.ignore_dst;
        }
        options.quick.ignore_timezone =
            matches!(self.criteria.timezone, Some(TimezoneCriterion::Ignore));
        options.quick.attributes = attribute_comparison(self.criteria.attrib);
        options.quick.unix_permissions = self.criteria.permissions;
        options.quick.owner = self.criteria.owner;
        options.quick.group = self.criteria.group;
        options.quick.version = self.criteria.version;
        options.content.ignore_unimportant = self.criteria.ignore_unimportant;
        match self.criteria.content {
            None | Some(ContentCriterion::Size) => options.content.enabled = false,
            Some(ContentCriterion::Crc) => {
                options.content.enabled = true;
                options.content.method = ContentMethod::Crc32;
            }
            Some(ContentCriterion::Binary) => {
                options.content.enabled = true;
                options.content.method = ContentMethod::Binary;
            }
            Some(ContentCriterion::RulesBased) => {
                options.content.enabled = true;
                options.content.method = ContentMethod::Rules;
            }
        }
        options
    }

    /// Install the clauses of one `filter` command.
    ///
    /// # Errors
    /// Returns [`ExecError::Refused`] for a date the clause cannot read.
    pub fn set_filters(&mut self, clauses: &[FilterClause]) -> Result<(), ExecError> {
        for clause in clauses {
            match clause {
                FilterClause::Masks(masks) => self.name_masks.clone_from(masks),
                FilterClause::Cutoff(spec) => {
                    self.other_filters.items.retain(|item| {
                        !matches!(
                            item,
                            OtherFilter::ModifiedOlderThan(_) | OtherFilter::ModifiedNewerThan(_)
                        )
                    });
                    if let Some(spec) = spec {
                        self.other_filters.items.push(self.cutoff_filter(spec)?);
                    }
                }
                FilterClause::Size(spec) => {
                    self.other_filters.items.retain(|item| {
                        !matches!(
                            item,
                            OtherFilter::SmallerThan(_) | OtherFilter::LargerThan(_)
                        )
                    });
                    if let Some(spec) = spec {
                        self.other_filters.items.push(if spec.larger {
                            OtherFilter::LargerThan(spec.bytes())
                        } else {
                            OtherFilter::SmallerThan(spec.bytes())
                        });
                    }
                }
                FilterClause::Attrib(change) => {
                    if let Some(change) = change {
                        let unsupported = change.attrs.compressed
                            || change.attrs.encrypted
                            || change.attrs.not_indexed
                            || change.attrs.link
                            || change.attrs.offline
                            || change.attrs.pinned
                            || change.attrs.temporary
                            || change.attrs.unpinned
                            || change.attrs.sparse;
                        if unsupported {
                            return Err(ExecError::not_supported(
                                "filter",
                                "the file system cannot read every named attribute",
                            ));
                        }
                    }
                    self.other_filters.items.retain(|item| {
                        !matches!(
                            item,
                            OtherFilter::AttributeSet(_) | OtherFilter::AttributeNotSet(_)
                        )
                    });
                    if let Some(change) = change {
                        for kind in attribute_kinds(change.attrs) {
                            self.other_filters.items.push(if change.include {
                                OtherFilter::AttributeNotSet(kind)
                            } else {
                                OtherFilter::AttributeSet(kind)
                            });
                        }
                    }
                }
                FilterClause::UnixType(_) => {
                    return Err(ExecError::not_supported(
                        "filter",
                        "the file kind clause has no engine behind it yet",
                    ))
                }
                FilterClause::ExcludeProtected => {
                    self.other_filters.exclude_protected_system = true;
                }
                FilterClause::IncludeProtected => {
                    self.other_filters.exclude_protected_system = false;
                }
            }
        }
        Ok(())
    }

    fn cutoff_filter(&self, spec: &CutoffSpec) -> Result<OtherFilter, ExecError> {
        let time = match &spec.value {
            CutoffValue::Days(days) => FilterTime::DaysAgo(*days),
            CutoffValue::Timestamp(text) => {
                let seconds = clock::parse_timestamp(text, self.options.offset_seconds)
                    .ok_or_else(|| {
                        ExecError::Refused(format!(
                            "the date {text} is not written as yyyy-mm-dd with an optional hh:mm:ss"
                        ))
                    })?;
                FilterTime::Absolute(clock::system_time(seconds).ok_or_else(|| {
                    ExecError::Refused(format!(
                        "the date {text} is outside the range this system can represent"
                    ))
                })?)
            }
        };
        Ok(if spec.newer {
            OtherFilter::ModifiedNewerThan(time)
        } else {
            OtherFilter::ModifiedOlderThan(time)
        })
    }

    /// Open or close folders.
    pub fn set_expanded(&mut self, paths: &PathsArg, open: bool) {
        match paths {
            PathsArg::All => {
                if open {
                    self.expanded.all = true;
                } else {
                    self.expanded.clear();
                }
            }
            PathsArg::Paths(list) => {
                for path in list {
                    let rel = PathBuf::from(path);
                    if open {
                        self.expanded.paths.insert(rel);
                    } else {
                        self.expanded.paths.remove(&rel);
                    }
                }
            }
        }
    }

    /// Replace the selection with everything the masks name.
    ///
    /// A folder is reached only when every folder above it is open, which is
    /// what makes `expand` a precondition of the file operations.
    pub fn select(&mut self, masks: &[SelectMask]) {
        let mut chosen: BTreeSet<PathBuf> = BTreeSet::new();
        if let Some(tree) = &self.tree {
            let mut stack: Vec<&Node> = tree.children.iter().collect();
            while let Some(node) = stack.pop() {
                if masks.iter().any(|mask| matches(*mask, node)) {
                    chosen.insert(node.rel.clone());
                }
                if node.is_dir && self.expanded.holds(&node.rel) {
                    stack.extend(node.children.iter());
                }
            }
        }
        self.selection = chosen;
    }
}

fn attribute_comparison(attrs: Option<AttrSet>) -> AttributeComparison {
    let Some(attrs) = attrs else {
        return AttributeComparison::default();
    };
    AttributeComparison {
        read_only: attrs.read_only,
        hidden: attrs.hidden,
        system: attrs.system,
        archive: attrs.archive,
    }
}

/// The quick tests a newly loaded folder comparison starts with.
pub(crate) fn default_comparison_criteria() -> Criteria {
    Criteria {
        timestamp: Some(TimestampCriterion {
            tolerance_seconds: Some(2),
            ignore_dst: false,
        }),
        ..Criteria::default()
    }
}

fn comparable_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            std::path::Component::RootDir => resolved.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if let Some(canonical) = canonicalize_if_present(&resolved)? {
                    resolved = canonical;
                }
                resolved.pop();
            }
            std::path::Component::Normal(name) => {
                if let Some(canonical) = canonicalize_if_present(&resolved)? {
                    resolved = canonical;
                }
                resolved.push(name);
            }
        }
    }
    if let Some(canonical) = canonicalize_if_present(&resolved)? {
        resolved = canonical;
    }
    #[cfg(windows)]
    {
        resolved = PathBuf::from(resolved.to_string_lossy().to_lowercase());
    }
    Ok(resolved)
}

fn canonicalize_if_present(path: &Path) -> std::io::Result<Option<PathBuf>> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(Some(canonical)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn attribute_kinds(attrs: crate::ast::FilterAttrSet) -> Vec<AttributeKind> {
    let mut out = Vec::new();
    if attrs.archive {
        out.push(AttributeKind::Archive);
    }
    if attrs.hidden {
        out.push(AttributeKind::Hidden);
    }
    if attrs.read_only {
        out.push(AttributeKind::ReadOnly);
    }
    if attrs.system {
        out.push(AttributeKind::System);
    }
    out
}

/// True when one mask names the node.
fn matches(mask: SelectMask, node: &Node) -> bool {
    match mask {
        SelectMask::EmptyFolders => node.is_dir && node.children.is_empty(),
        SelectMask::Mask { side, result, kind } => {
            side_matches(side, node)
                && result_matches(result, side, node)
                && kind_matches(kind, node)
        }
    }
}

fn side_matches(side: SideArg, node: &Node) -> bool {
    match side {
        SideArg::All => true,
        SideArg::Left => node.left.is_some(),
        SideArg::Right => node.right.is_some(),
    }
}

fn kind_matches(kind: SelectKind, node: &Node) -> bool {
    match kind {
        SelectKind::All => true,
        SelectKind::Files => !node.is_dir,
        SelectKind::Folders => node.is_dir,
    }
}

/// A folder answers a result mask only when everything below it answers it,
/// which is what keeps a mixed folder out of a narrow selection.
fn result_matches(result: SelectResult, side: SideArg, node: &Node) -> bool {
    if result == SelectResult::All {
        return true;
    }
    if node.is_dir {
        if node.children.is_empty() {
            return status_matches(result, side, node.status);
        }
        return node
            .children
            .iter()
            .all(|child| result_matches(result, side, child));
    }
    status_matches(result, side, node.status)
}

/// `newer` and `older` read against the side the mask names. A mask with no
/// side reads them against the left side.
fn status_matches(result: SelectResult, side: SideArg, status: NodeStatus) -> bool {
    let (newer, older) = match side {
        SideArg::Right => (NodeStatus::RightNewer, NodeStatus::LeftNewer),
        SideArg::Left | SideArg::All => (NodeStatus::LeftNewer, NodeStatus::RightNewer),
    };
    match result {
        SelectResult::All => true,
        SelectResult::Exact => status == NodeStatus::Same,
        SelectResult::Diff => matches!(status, NodeStatus::Different | NodeStatus::KindMismatch),
        SelectResult::Newer => status == newer,
        SelectResult::Older => status == older,
        SelectResult::Orphan => status.is_orphan(),
    }
}
