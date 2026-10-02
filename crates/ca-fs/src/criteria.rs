//! Quick tests and content tests that decide whether two entries match.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::cancel::Cancel;
use crate::filter::unix_seconds;
use crate::scan::Entry;
use crate::source::{EntryFacts, Source};

/// Which side of a comparison an observation belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Left side.
    Left,
    /// Right side.
    Right,
}

/// Which attributes take part in the attribute quick test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttributeComparison {
    /// Compare the read-only flag.
    pub read_only: bool,
    /// Compare the hidden flag.
    pub hidden: bool,
    /// Compare the system flag.
    pub system: bool,
    /// Compare the archive flag.
    pub archive: bool,
}

impl AttributeComparison {
    /// True when no attribute takes part.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !(self.read_only || self.hidden || self.system || self.archive)
    }
}

/// Tests answerable from a directory listing alone.
///
/// Copying a file can change its archive flag, so the attribute set is empty
/// by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickTests {
    /// Compare byte sizes.
    pub size: bool,
    /// Compare last modified times.
    pub timestamp: bool,
    /// Timestamps must differ by more than this many seconds to count.
    pub tolerance_seconds: u32,
    /// Treat an exact one hour difference as a match.
    pub ignore_daylight_saving: bool,
    /// Treat any exact whole hour difference as a match.
    pub ignore_timezone: bool,
    /// Compare the capitalization of the name.
    pub filename_case: bool,
    /// Attributes taking part in the comparison.
    pub attributes: AttributeComparison,
    /// Compare Unix permission bits.
    pub unix_permissions: bool,
    /// Compare Unix owner ids.
    pub owner: bool,
    /// Compare Unix group ids.
    pub group: bool,
    /// Compare embedded module version information.
    pub version: bool,
}

impl Default for QuickTests {
    fn default() -> Self {
        Self {
            size: true,
            timestamp: true,
            tolerance_seconds: 0,
            ignore_daylight_saving: false,
            ignore_timezone: false,
            filename_case: false,
            attributes: AttributeComparison::default(),
            unix_permissions: false,
            owner: false,
            group: false,
            version: false,
        }
    }
}

/// A single reason a quick test reported a mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickDifference {
    /// Byte sizes differ.
    Size,
    /// Last modified times differ by more than the tolerance allows.
    Timestamp,
    /// Names differ only in capitalization.
    FilenameCase,
    /// A compared attribute differs.
    Attributes,
    /// Unix permission bits differ.
    Permissions,
    /// Unix owner ids differ.
    Owner,
    /// Unix group ids differ.
    Group,
    /// Embedded version information differs.
    Version,
}

/// Outcome of the quick tests for one pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuickResult {
    /// Every test that reported a mismatch.
    pub differences: Vec<QuickDifference>,
    /// The side holding the later timestamp, when timestamps were compared and
    /// differ by more than the tolerance.
    pub newer: Option<Side>,
    /// Set when the size test or the timestamp test was asked for and neither
    /// could compare anything: no side's size is proven and neither side
    /// holds a time stamp. No difference is then no evidence that the two
    /// sides match. Never set together with a difference.
    pub unsettled: bool,
}

impl QuickResult {
    /// True when the quick tests compared the pair and none reported a
    /// mismatch.
    #[must_use]
    pub fn is_same(&self) -> bool {
        self.differences.is_empty() && !self.unsettled
    }

    /// True when no quick test reported a mismatch and the tests asked for had
    /// nothing to compare, so the listing alone decides nothing.
    #[must_use]
    pub fn is_unsettled(&self) -> bool {
        self.unsettled
    }
}

/// Run the quick tests over one pair of entries.
///
/// Both sides are read as a local listing reads them: sizes are exact and time
/// stamps name an instant in UTC.
#[must_use]
pub fn quick_compare(left: &Entry, right: &Entry, tests: &QuickTests) -> QuickResult {
    quick_compare_with(
        left,
        EntryFacts::default(),
        right,
        EntryFacts::default(),
        tests,
    )
}

/// Run the quick tests over one pair of entries, honouring what each side's
/// source states about its own precision.
///
/// A size that either side could not prove settles nothing, so the size test
/// is skipped for that pair rather than reporting a difference. Time stamps
/// are compared under the coarser of the two sides' precisions plus the
/// configured tolerance, so a container that records a two second tick never
/// reports a difference against a folder that records an instant.
///
/// When the size test or the timestamp test is asked for and neither finds
/// anything to compare, the result is [`QuickResult::unsettled`]: the other
/// tests read metadata only, so their silence proves nothing about the
/// content.
#[must_use]
pub fn quick_compare_with(
    left: &Entry,
    left_facts: EntryFacts,
    right: &Entry,
    right_facts: EntryFacts,
    tests: &QuickTests,
) -> QuickResult {
    let mut result = QuickResult::default();

    let sizes_are_provable = left_facts.size_is_exact && right_facts.size_is_exact;
    let sizes_compared = tests.size && sizes_are_provable && !left.is_dir && !right.is_dir;
    if sizes_compared && left.size != right.size {
        result.differences.push(QuickDifference::Size);
    }
    let times_compared = tests.timestamp && (left.modified.is_some() || right.modified.is_some());

    if tests.timestamp {
        let tolerance = u32::try_from(
            left_facts
                .time_fidelity
                .tolerance_seconds()
                .max(right_facts.time_fidelity.tolerance_seconds()),
        )
        .unwrap_or(u32::MAX);
        let tests = &QuickTests {
            tolerance_seconds: tests.tolerance_seconds.max(tolerance),
            ..tests.clone()
        };
        match (left.modified, right.modified) {
            (Some(left_time), Some(right_time)) => {
                let left_seconds = unix_seconds(left_time);
                let right_seconds = unix_seconds(right_time);
                if !timestamps_match(left_seconds, right_seconds, tests) {
                    result.differences.push(QuickDifference::Timestamp);
                    result.newer = Some(if left_seconds > right_seconds {
                        Side::Left
                    } else {
                        Side::Right
                    });
                }
            }
            (None, None) => {}
            _ => result.differences.push(QuickDifference::Timestamp),
        }
    }

    if tests.filename_case && left.name != right.name && left.name.eq_ignore_ascii_case(&right.name)
    {
        result.differences.push(QuickDifference::FilenameCase);
    }

    if !tests.attributes.is_empty() && attributes_differ(left, right, tests.attributes) {
        result.differences.push(QuickDifference::Attributes);
    }

    if tests.unix_permissions
        && left.attributes.permission_bits() != right.attributes.permission_bits()
    {
        result.differences.push(QuickDifference::Permissions);
    }
    if tests.owner && left.attributes.uid != right.attributes.uid {
        result.differences.push(QuickDifference::Owner);
    }
    if tests.group && left.attributes.gid != right.attributes.gid {
        result.differences.push(QuickDifference::Group);
    }

    result.unsettled = result.differences.is_empty()
        && (tests.size || tests.timestamp)
        && !sizes_compared
        && !times_compared;
    result
}

/// True when two timestamps, in seconds since the Unix epoch, count as equal
/// under the configured tolerance and hour-offset allowances.
///
/// The timezone allowance forgives whole hour offsets only, so a zone at a
/// half or quarter hour offset still reports a mismatch.
///
/// A difference too large to express as a signed count of seconds cannot be
/// inside any tolerance, so it never matches.
#[must_use]
pub fn timestamps_match(left: i64, right: i64, tests: &QuickTests) -> bool {
    let tolerance = u64::from(tests.tolerance_seconds);
    let Some(delta) = left.checked_sub(right).map(i64::unsigned_abs) else {
        return false;
    };
    if delta <= tolerance {
        return true;
    }
    if tests.ignore_daylight_saving && delta.abs_diff(3_600) <= tolerance {
        return true;
    }
    if tests.ignore_timezone {
        let remainder = delta % 3_600;
        if remainder <= tolerance || 3_600 - remainder <= tolerance {
            return true;
        }
    }
    false
}

fn attributes_differ(left: &Entry, right: &Entry, compared: AttributeComparison) -> bool {
    let left_bits = &left.attributes;
    let right_bits = &right.attributes;
    (compared.read_only && left_bits.read_only != right_bits.read_only)
        || (compared.hidden && left_bits.hidden != right_bits.hidden)
        || (compared.system && left_bits.system != right_bits.system)
        || (compared.archive && left_bits.archive != right_bits.archive)
}

/// Embedded module version information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileVersion {
    /// Major field.
    pub major: u16,
    /// Minor field.
    pub minor: u16,
    /// Maintenance field.
    pub maintenance: u16,
    /// Build field.
    pub build: u16,
}

/// Read embedded version information from an executable module.
///
/// Version resources exist only on Windows, and reading them is not
/// implemented, so this always reports `None` and the version quick test never
/// contributes a difference.
#[must_use]
pub fn file_version(_path: &Path) -> Option<FileVersion> {
    None
}

/// How a content comparison reads the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentMethod {
    /// Compare 32 bit checksums of the whole file.
    #[default]
    Crc32,
    /// Compare byte by byte, stopping at the first difference.
    Binary,
    /// Compare through a format aware engine that can classify differences as
    /// unimportant.
    Rules,
}

/// Result of a content comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentOutcome {
    /// The files are byte identical.
    BinarySame,
    /// At least one byte differs.
    BinaryDifferences,
    /// Bytes differ but every difference is ignorable, such as the encoding.
    RulesSame,
    /// Only differences classified as unimportant were found.
    UnimportantDifferences,
    /// Differences classified as important were found.
    ImportantDifferences,
    /// At least one side is a cloud placeholder whose data is not resident, so
    /// no content test ran.
    NotComparedCloudPlaceholder,
}

impl ContentOutcome {
    /// True when the outcome counts as a match.
    ///
    /// Unimportant differences count as a match only when the caller is
    /// ignoring them.
    #[must_use]
    pub fn is_same(self, ignore_unimportant: bool) -> bool {
        match self {
            Self::BinarySame | Self::RulesSame => true,
            Self::UnimportantDifferences => ignore_unimportant,
            Self::BinaryDifferences
            | Self::ImportantDifferences
            | Self::NotComparedCloudPlaceholder => false,
        }
    }

    /// True when no content test ran, so the pair keeps whatever status the
    /// quick tests gave it.
    #[must_use]
    pub fn is_not_compared(self) -> bool {
        matches!(self, Self::NotComparedCloudPlaceholder)
    }
}

/// A format aware comparison supplied by the caller.
///
/// Implementations must be safe to call from several worker threads at once.
pub trait RulesComparer: Send + Sync {
    /// Compare two files and classify the differences.
    ///
    /// The flag is polled between units of work, so a long comparison stops
    /// without waiting for the whole file.
    ///
    /// # Errors
    /// Returns an error when either file cannot be read or the comparison is
    /// cancelled.
    fn compare(
        &self,
        left: &Path,
        right: &Path,
        cancel: &Cancel,
    ) -> Result<ContentOutcome, ContentError>;

    /// Compare two entries the caller already read, and classify the
    /// differences.
    ///
    /// `name` chooses the file format the rules follow. An entry inside a
    /// container has no local path, so its bytes arrive here instead.
    ///
    /// # Errors
    /// Returns an error when the comparison is cancelled.
    fn compare_bytes(
        &self,
        name: &Path,
        left: &[u8],
        right: &[u8],
        cancel: &Cancel,
    ) -> Result<ContentOutcome, ContentError>;
}

/// Errors raised by a content comparison.
#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    /// A file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file being read.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The comparison stopped because the cancel flag was set.
    #[error("comparison cancelled")]
    Cancelled,
    /// A rules based comparison was requested without an engine to run it.
    #[error("no rules engine supplied")]
    RulesUnavailable,
    /// The source records the entry but not its bytes, so no content test can
    /// run. A recorded listing is the usual case.
    #[error("{path} has no stored content in {label}")]
    ContentNotStored {
        /// The entry that holds no bytes.
        path: String,
        /// Label of the source that records it.
        label: String,
    },
    /// A source refused the read for a reason of its own, such as a ceiling
    /// reached while expanding a container.
    #[error("cannot read {path}: {detail}")]
    Source {
        /// The entry being read.
        path: String,
        /// Text of the underlying failure.
        detail: String,
    },
}

/// Content comparison settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentTests {
    /// Run a content comparison when the session loads.
    pub enabled: bool,
    /// How the content is read.
    pub method: ContentMethod,
    /// Skip the content test when the quick tests already report a match.
    pub skip_if_quick_same: bool,
    /// Let a content match override a quick test mismatch.
    pub override_quick: bool,
    /// Treat unimportant differences as a match.
    pub ignore_unimportant: bool,
    /// Read buffer size for the byte comparison.
    pub buffer_size: usize,
    /// A file this size or larger is compared as bytes even when the method is
    /// [`ContentMethod::Rules`]. Decoding a large file as text costs memory
    /// proportional to its size.
    pub binary_threshold_bytes: u64,
    /// Leave cloud placeholders uncompared. Reading one makes the provider
    /// download the whole file.
    pub skip_cloud_placeholders: bool,
}

impl Default for ContentTests {
    fn default() -> Self {
        Self {
            enabled: false,
            method: ContentMethod::Crc32,
            skip_if_quick_same: true,
            override_quick: true,
            ignore_unimportant: false,
            buffer_size: 64 * 1024,
            binary_threshold_bytes: 4 * 1024 * 1024,
            skip_cloud_placeholders: true,
        }
    }
}

/// Every comparison setting of a folder session that this crate acts on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompareOptions {
    /// Quick tests.
    pub quick: QuickTests,
    /// Content tests.
    pub content: ContentTests,
}

/// Compute the checksum of a file, stopping early when cancelled.
///
/// # Errors
/// Returns [`ContentError::Io`] when the file cannot be read and
/// [`ContentError::Cancelled`] when the flag is set mid-read.
pub fn crc32(path: &Path, cancel: &Cancel, buffer_size: usize) -> Result<u32, ContentError> {
    let file = open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = vec![0_u8; buffer_size.max(4096)];
    loop {
        if cancel.is_cancelled() {
            return Err(ContentError::Cancelled);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|source| ContentError::Io {
                path: path.display().to_string(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

/// Compare two files byte by byte, returning as soon as a difference is found.
///
/// # Errors
/// Returns [`ContentError::Io`] when either file cannot be read and
/// [`ContentError::Cancelled`] when the flag is set mid-read.
pub fn binary_equal(
    left: &Path,
    right: &Path,
    cancel: &Cancel,
    buffer_size: usize,
) -> Result<bool, ContentError> {
    let left_file = open(left)?;
    let right_file = open(right)?;
    let left_length = left_file.metadata().map(|meta| meta.len()).ok();
    let right_length = right_file.metadata().map(|meta| meta.len()).ok();
    if let (Some(left_length), Some(right_length)) = (left_length, right_length) {
        if left_length != right_length {
            return Ok(false);
        }
    }

    let size = buffer_size.max(4096);
    let mut left_reader = BufReader::new(left_file);
    let mut right_reader = BufReader::new(right_file);
    let mut left_buffer = vec![0_u8; size];
    let mut right_buffer = vec![0_u8; size];
    loop {
        if cancel.is_cancelled() {
            return Err(ContentError::Cancelled);
        }
        let left_read = fill(&mut left_reader, &mut left_buffer, left)?;
        let right_read = fill(&mut right_reader, &mut right_buffer, right)?;
        if left_read != right_read {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
        if left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
    }
}

/// True when the platform marks the file's data as not resident, so opening it
/// would make a cloud provider download it.
#[must_use]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const OFFLINE: u32 = 0x0000_1000;
        const RECALL_ON_OPEN: u32 = 0x0004_0000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        std::fs::metadata(path).is_ok_and(|meta| {
            meta.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// Run the configured content comparison over one pair of files.
///
/// A rules based comparison runs the byte comparison first, which is why it can
/// report that the files are byte identical. A checksum comparison settles a
/// size mismatch without reading either file.
///
/// # Errors
/// Propagates read errors, cancellation, and the absence of a rules engine.
pub fn compare_contents(
    left: &Path,
    right: &Path,
    tests: &ContentTests,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> Result<ContentOutcome, ContentError> {
    if tests.skip_cloud_placeholders && (is_cloud_placeholder(left) || is_cloud_placeholder(right))
    {
        return Ok(ContentOutcome::NotComparedCloudPlaceholder);
    }
    let buffer_size = tests.buffer_size;
    match tests.method {
        ContentMethod::Crc32 => {
            if sizes_differ(left, right) {
                return Ok(ContentOutcome::BinaryDifferences);
            }
            let left_crc = crc32(left, cancel, buffer_size)?;
            let right_crc = crc32(right, cancel, buffer_size)?;
            Ok(if left_crc == right_crc {
                ContentOutcome::BinarySame
            } else {
                ContentOutcome::BinaryDifferences
            })
        }
        ContentMethod::Binary => Ok(if binary_equal(left, right, cancel, buffer_size)? {
            ContentOutcome::BinarySame
        } else {
            ContentOutcome::BinaryDifferences
        }),
        ContentMethod::Rules => {
            if binary_equal(left, right, cancel, buffer_size)? {
                return Ok(ContentOutcome::BinarySame);
            }
            if above_threshold(left, right, tests.binary_threshold_bytes) {
                return Ok(ContentOutcome::BinaryDifferences);
            }
            let engine = rules.ok_or(ContentError::RulesUnavailable)?;
            engine.compare(left, right, cancel)
        }
    }
}

/// One side of a content comparison: where the entry is, and what its listing
/// stated about itself.
#[derive(Debug, Clone, Copy)]
pub struct ContentSide<'a> {
    /// The source holding the entry.
    pub source: &'a Source,
    /// The entry's path inside that source.
    pub path: &'a ca_vfs::VfsPath,
    /// What the listing stated about the entry.
    pub facts: EntryFacts,
}

/// Run a content comparison over one pair of entries held in any two sources.
///
/// A source that stores no bytes fails with [`ContentError::ContentNotStored`]
/// rather than reporting a match or a difference.
///
/// A checksum the two sources stored with their own entries is used in one
/// direction only: two checksums that disagree prove the bytes differ, and the
/// pair is reported different without either file being read. Two checksums
/// that agree prove nothing, so the bytes are read anyway. A size either side
/// could not prove is not read as evidence at all.
///
/// The flag is polled between buffers, so a comparison of a large entry inside
/// a container stops without finishing the entry.
///
/// # Errors
/// Returns [`ContentError::ContentNotStored`] for a listing-only source,
/// [`ContentError::Source`] when a source refuses the read, and
/// [`ContentError::Cancelled`] when the flag is set mid-read.
pub fn compare_source_contents(
    left: ContentSide<'_>,
    right: ContentSide<'_>,
    tests: &ContentTests,
    cancel: &Cancel,
) -> Result<ContentOutcome, ContentError> {
    let (left_path, right_path) = (left.path, right.path);
    for side in [&left, &right] {
        if !side.source.has_content() {
            return Err(ContentError::ContentNotStored {
                path: side.path.as_str().to_owned(),
                label: side.source.label().to_owned(),
            });
        }
    }
    if let (Some(left_crc), Some(right_crc)) = (left.facts.crc32, right.facts.crc32) {
        if left_crc != right_crc {
            return Ok(ContentOutcome::BinaryDifferences);
        }
    }
    let (left, right) = (left.source, right.source);

    let vfs_cancel = ca_vfs::Cancel::from_flag(cancel.as_flag());
    let mut left_reader = open_in(left, left_path, &vfs_cancel)?;
    let mut right_reader = open_in(right, right_path, &vfs_cancel)?;
    let size = tests.buffer_size.max(4096);
    let mut left_buffer = vec![0_u8; size];
    let mut right_buffer = vec![0_u8; size];
    loop {
        if cancel.is_cancelled() {
            return Err(ContentError::Cancelled);
        }
        let left_read = fill_source(&mut left_reader, &mut left_buffer, left, left_path)?;
        let right_read = fill_source(&mut right_reader, &mut right_buffer, right, right_path)?;
        if left_read != right_read {
            return Ok(ContentOutcome::BinaryDifferences);
        }
        if left_read == 0 {
            return Ok(ContentOutcome::BinarySame);
        }
        if left_buffer.get(..left_read) != right_buffer.get(..right_read) {
            return Ok(ContentOutcome::BinaryDifferences);
        }
    }
}

/// Run the configured content comparison over one pair of entries held in
/// any two sources, the rules method included.
///
/// The rules method runs the byte comparison first, as it does for local
/// files. A pair that differs is then read in full through the sources and
/// handed to `rules`. A pair at or above the binary threshold stays a byte
/// comparison.
///
/// # Errors
/// Returns the errors of [`compare_source_contents`], and
/// [`ContentError::RulesUnavailable`] when the method is rules and no engine
/// is given.
pub fn compare_source_contents_with(
    left: ContentSide<'_>,
    right: ContentSide<'_>,
    tests: &ContentTests,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> Result<ContentOutcome, ContentError> {
    let outcome = compare_source_contents(left, right, tests, cancel)?;
    if tests.method != ContentMethod::Rules || outcome != ContentOutcome::BinaryDifferences {
        return Ok(outcome);
    }
    let engine = rules.ok_or(ContentError::RulesUnavailable)?;
    let threshold = tests.binary_threshold_bytes;
    let Some(left_bytes) = read_source_below(left, threshold, cancel)? else {
        return Ok(ContentOutcome::BinaryDifferences);
    };
    let Some(right_bytes) = read_source_below(right, threshold, cancel)? else {
        return Ok(ContentOutcome::BinaryDifferences);
    };
    engine.compare_bytes(
        Path::new(left.path.as_str()),
        &left_bytes,
        &right_bytes,
        cancel,
    )
}

/// The whole entry, or `None` when it holds `threshold` bytes or more.
fn read_source_below(
    side: ContentSide<'_>,
    threshold: u64,
    cancel: &Cancel,
) -> Result<Option<Vec<u8>>, ContentError> {
    use std::io::Read;
    if threshold == 0 {
        return Ok(None);
    }
    let vfs_cancel = ca_vfs::Cancel::from_flag(cancel.as_flag());
    let reader = open_in(side.source, side.path, &vfs_cancel)?;
    let mut bytes = Vec::new();
    reader
        .take(threshold)
        .read_to_end(&mut bytes)
        .map_err(|error| ContentError::Source {
            path: side.path.as_str().to_owned(),
            detail: error.to_string(),
        })?;
    if cancel.is_cancelled() {
        return Err(ContentError::Cancelled);
    }
    Ok((u64::try_from(bytes.len()).unwrap_or(u64::MAX) < threshold).then_some(bytes))
}

fn open_in(
    source: &Source,
    path: &ca_vfs::VfsPath,
    cancel: &ca_vfs::Cancel,
) -> Result<ca_vfs::OpenFile, ContentError> {
    source
        .file_system()
        .open(path, cancel)
        .map_err(|error| match error {
            ca_vfs::VfsError::Cancelled => ContentError::Cancelled,
            ca_vfs::VfsError::ContentNotStored { .. } => ContentError::ContentNotStored {
                path: path.as_str().to_owned(),
                label: source.label().to_owned(),
            },
            other => ContentError::Source {
                path: path.as_str().to_owned(),
                detail: other.to_string(),
            },
        })
}

/// Read until the buffer is full or the entry ends, so two readers advance in
/// step whatever size the source hands back per call.
fn fill_source(
    reader: &mut ca_vfs::OpenFile,
    buffer: &mut [u8],
    source: &Source,
    path: &ca_vfs::VfsPath,
) -> Result<usize, ContentError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let slice = buffer.get_mut(filled..).unwrap_or_default();
        let read = reader.read(slice).map_err(|error| ContentError::Source {
            path: format!("{}/{}", source.label(), path.as_str()),
            detail: error.to_string(),
        })?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// True when either side reaches the size at which content stops being read as
/// text. A threshold of zero turns the rules pass off for every file.
fn above_threshold(left: &Path, right: &Path, threshold: u64) -> bool {
    if threshold == 0 {
        return true;
    }
    [left, right]
        .into_iter()
        .any(|path| std::fs::metadata(path).is_ok_and(|meta| meta.len() >= threshold))
}

/// True only when both sizes are known and disagree, so an unreadable file
/// still reaches the comparison and reports its own error.
fn sizes_differ(left: &Path, right: &Path) -> bool {
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(left_meta), Ok(right_meta)) => left_meta.len() != right_meta.len(),
        _ => false,
    }
}

fn open(path: &Path) -> Result<File, ContentError> {
    File::open(path).map_err(|source| ContentError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Read until the buffer is full or the file ends, so two readers advance in
/// step regardless of how the file system splits reads.
fn fill(
    reader: &mut BufReader<File>,
    buffer: &mut [u8],
    path: &Path,
) -> Result<usize, ContentError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = reader
            .read(&mut buffer[filled..])
            .map_err(|source| ContentError::Io {
                path: path.display().to_string(),
                source,
            })?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        binary_equal, compare_contents, crc32, file_version, is_cloud_placeholder, quick_compare,
        quick_compare_with, timestamps_match, AttributeComparison, ContentError, ContentMethod,
        ContentOutcome, ContentTests, EntryFacts, QuickDifference, QuickTests, RulesComparer, Side,
    };
    use crate::cancel::Cancel;
    use crate::filter::from_unix_seconds;
    use crate::scan::{Attributes, Entry};
    use std::path::{Path, PathBuf};

    fn entry(name: &str, size: u64, modified: i64) -> Entry {
        Entry {
            rel: PathBuf::from(name),
            name: name.to_owned(),
            is_dir: false,
            size,
            modified: Some(from_unix_seconds(modified).unwrap_or(std::time::UNIX_EPOCH)),
            created: None,
            attributes: Attributes::default(),
            link: None,
            error: None,
            listing_incomplete: false,
            refused: false,
        }
    }

    fn tests_for(method: ContentMethod) -> ContentTests {
        ContentTests {
            method,
            buffer_size: 4096,
            ..ContentTests::default()
        }
    }

    #[test]
    fn size_and_timestamp_are_the_default_quick_tests() {
        let tests = QuickTests::default();
        let result = quick_compare(&entry("a", 1, 100), &entry("a", 2, 100), &tests);
        assert_eq!(result.differences, vec![QuickDifference::Size]);

        let result = quick_compare(&entry("a", 1, 100), &entry("a", 1, 200), &tests);
        assert_eq!(result.differences, vec![QuickDifference::Timestamp]);
        assert_eq!(result.newer, Some(Side::Right));

        let result = quick_compare(&entry("a", 1, 300), &entry("a", 1, 200), &tests);
        assert_eq!(result.newer, Some(Side::Left));
    }

    #[test]
    fn a_pair_with_nothing_to_compare_is_unsettled_rather_than_the_same() {
        let mut left = entry("a", 1, 100);
        left.modified = None;
        let mut right = entry("a", 2, 100);
        right.modified = None;
        let inexact = EntryFacts {
            size_is_exact: false,
            ..EntryFacts::default()
        };
        let result = quick_compare_with(&left, inexact, &right, inexact, &QuickTests::default());
        assert!(result.differences.is_empty());
        assert!(result.is_unsettled() && !result.is_same());

        let asked_for_neither = QuickTests {
            size: false,
            timestamp: false,
            ..QuickTests::default()
        };
        assert!(quick_compare_with(&left, inexact, &right, inexact, &asked_for_neither).is_same());

        let timed = entry("a", 2, 100);
        let result = quick_compare_with(&left, inexact, &timed, inexact, &QuickTests::default());
        assert_eq!(result.differences, vec![QuickDifference::Timestamp]);
        assert!(!result.is_unsettled());
    }

    #[test]
    fn tolerance_absorbs_small_timestamp_differences() {
        let tests = QuickTests {
            tolerance_seconds: 2,
            ..QuickTests::default()
        };
        assert!(timestamps_match(100, 102, &tests));
        assert!(!timestamps_match(100, 103, &tests));
        assert!(quick_compare(&entry("a", 1, 100), &entry("a", 1, 102), &tests).is_same());
    }

    #[test]
    fn daylight_saving_ignores_one_hour_only() {
        let tests = QuickTests {
            ignore_daylight_saving: true,
            ..QuickTests::default()
        };
        assert!(timestamps_match(0, 3_600, &tests));
        assert!(timestamps_match(3_600, 0, &tests));
        assert!(!timestamps_match(0, 7_200, &tests));
        assert!(!timestamps_match(0, 3_601, &tests));
    }

    #[test]
    fn daylight_saving_respects_tolerance() {
        let tests = QuickTests {
            ignore_daylight_saving: true,
            tolerance_seconds: 2,
            ..QuickTests::default()
        };
        assert!(timestamps_match(0, 3_602, &tests));
        assert!(!timestamps_match(0, 3_605, &tests));
    }

    #[test]
    fn timezone_ignores_whole_hour_multiples() {
        let tests = QuickTests {
            ignore_timezone: true,
            ..QuickTests::default()
        };
        assert!(timestamps_match(0, 7_200, &tests));
        assert!(timestamps_match(0, 3_600 * 13, &tests));
        assert!(!timestamps_match(0, 1_800, &tests));
    }

    #[test]
    fn filename_case_test_only_fires_on_case_only_differences() {
        let tests = QuickTests {
            filename_case: true,
            size: false,
            timestamp: false,
            ..QuickTests::default()
        };
        let result = quick_compare(&entry("File.txt", 1, 0), &entry("file.txt", 1, 0), &tests);
        assert_eq!(result.differences, vec![QuickDifference::FilenameCase]);
        let result = quick_compare(&entry("a.txt", 1, 0), &entry("b.txt", 1, 0), &tests);
        assert!(result.is_same());
    }

    #[test]
    fn attribute_test_honours_the_selected_set() {
        let mut left = entry("a", 1, 0);
        left.attributes.archive = true;
        let right = entry("a", 1, 0);
        let tests = QuickTests {
            size: false,
            timestamp: false,
            attributes: AttributeComparison {
                archive: true,
                ..AttributeComparison::default()
            },
            ..QuickTests::default()
        };
        assert_eq!(
            quick_compare(&left, &right, &tests).differences,
            vec![QuickDifference::Attributes]
        );
        let ignoring = QuickTests {
            size: false,
            timestamp: false,
            ..QuickTests::default()
        };
        assert!(quick_compare(&left, &right, &ignoring).is_same());
    }

    #[test]
    fn version_lookup_is_a_stub() {
        assert!(file_version(Path::new("a.exe")).is_none());
    }

    struct AlwaysUnimportant;
    impl RulesComparer for AlwaysUnimportant {
        fn compare(
            &self,
            _left: &Path,
            _right: &Path,
            _cancel: &Cancel,
        ) -> Result<ContentOutcome, ContentError> {
            Ok(ContentOutcome::UnimportantDifferences)
        }

        fn compare_bytes(
            &self,
            _name: &Path,
            _left: &[u8],
            _right: &[u8],
            _cancel: &Cancel,
        ) -> Result<ContentOutcome, ContentError> {
            Ok(ContentOutcome::UnimportantDifferences)
        }
    }

    #[test]
    fn the_binary_threshold_keeps_a_large_file_away_from_the_rules_engine() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("l.txt");
        let right = dir.path().join("r.txt");
        std::fs::write(&left, b"one").unwrap();
        std::fs::write(&right, b"two").unwrap();
        let cancel = Cancel::new();
        let engine = AlwaysUnimportant;

        let mut tests = tests_for(ContentMethod::Rules);
        tests.binary_threshold_bytes = 1_024;
        assert_eq!(
            compare_contents(&left, &right, &tests, Some(&engine), &cancel).unwrap(),
            ContentOutcome::UnimportantDifferences
        );

        tests.binary_threshold_bytes = 2;
        assert_eq!(
            compare_contents(&left, &right, &tests, Some(&engine), &cancel).unwrap(),
            ContentOutcome::BinaryDifferences,
            "a file at the threshold is read as bytes"
        );
    }

    #[test]
    fn content_methods_agree_on_identical_files() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("l.bin");
        let right = dir.path().join("r.bin");
        std::fs::write(&left, vec![7_u8; 200_000]).unwrap();
        std::fs::write(&right, vec![7_u8; 200_000]).unwrap();
        let cancel = Cancel::new();

        assert_eq!(
            crc32(&left, &cancel, 4096).unwrap(),
            crc32(&right, &cancel, 4096).unwrap()
        );
        assert!(binary_equal(&left, &right, &cancel, 4096).unwrap());
        for method in [
            ContentMethod::Crc32,
            ContentMethod::Binary,
            ContentMethod::Rules,
        ] {
            assert_eq!(
                compare_contents(&left, &right, &tests_for(method), None, &cancel).unwrap(),
                ContentOutcome::BinarySame
            );
        }
    }

    #[test]
    fn binary_compare_detects_differences() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("l.bin");
        let right = dir.path().join("r.bin");
        std::fs::write(&left, b"abcdef").unwrap();
        std::fs::write(&right, b"abcdeg").unwrap();
        let cancel = Cancel::new();
        assert!(!binary_equal(&left, &right, &cancel, 4096).unwrap());

        std::fs::write(&right, b"abcde").unwrap();
        assert!(!binary_equal(&left, &right, &cancel, 4096).unwrap());
        assert_eq!(
            compare_contents(
                &left,
                &right,
                &tests_for(ContentMethod::Crc32),
                None,
                &cancel
            )
            .unwrap(),
            ContentOutcome::BinaryDifferences
        );
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn checksum_settles_a_size_mismatch_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("l.bin");
        let right = dir.path().join("r.bin");
        std::fs::write(&left, vec![3_u8; 64 * 1024 * 1024]).unwrap();
        std::fs::write(&right, b"tiny").unwrap();
        let start = std::time::Instant::now();
        assert_eq!(
            compare_contents(
                &left,
                &right,
                &tests_for(ContentMethod::Crc32),
                None,
                &Cancel::new()
            )
            .unwrap(),
            ContentOutcome::BinaryDifferences
        );
        assert!(start.elapsed() < std::time::Duration::from_millis(250));
    }

    #[test]
    fn a_plain_file_is_not_a_cloud_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        std::fs::write(&file, b"x").unwrap();
        assert!(!is_cloud_placeholder(&file));
        assert!(ContentTests::default().skip_cloud_placeholders);
        assert!(ContentOutcome::NotComparedCloudPlaceholder.is_not_compared());
        assert!(!ContentOutcome::NotComparedCloudPlaceholder.is_same(true));
    }

    #[test]
    fn a_clamped_timestamp_difference_does_not_match() {
        let tests = QuickTests {
            tolerance_seconds: 2,
            ignore_timezone: true,
            ..QuickTests::default()
        };
        assert!(!timestamps_match(i64::MIN, i64::MAX, &tests));
        assert!(!timestamps_match(i64::MAX, i64::MIN, &tests));
    }

    #[test]
    fn rules_method_falls_through_to_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("l.txt");
        let right = dir.path().join("r.txt");
        std::fs::write(&left, b"one").unwrap();
        std::fs::write(&right, b"two").unwrap();
        let cancel = Cancel::new();
        let engine = AlwaysUnimportant;
        assert_eq!(
            compare_contents(
                &left,
                &right,
                &tests_for(ContentMethod::Rules),
                Some(&engine),
                &cancel
            )
            .unwrap(),
            ContentOutcome::UnimportantDifferences
        );
        assert!(matches!(
            compare_contents(
                &left,
                &right,
                &tests_for(ContentMethod::Rules),
                None,
                &cancel
            ),
            Err(ContentError::RulesUnavailable)
        ));
    }

    #[test]
    fn unimportant_differences_follow_the_ignore_switch() {
        assert!(!ContentOutcome::UnimportantDifferences.is_same(false));
        assert!(ContentOutcome::UnimportantDifferences.is_same(true));
        assert!(ContentOutcome::RulesSame.is_same(false));
        assert!(!ContentOutcome::ImportantDifferences.is_same(true));
    }

    #[test]
    fn cancelled_content_comparison_reports_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("l.bin");
        let right = dir.path().join("r.bin");
        std::fs::write(&left, vec![0_u8; 8192]).unwrap();
        std::fs::write(&right, vec![0_u8; 8192]).unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(matches!(
            crc32(&left, &cancel, 4096),
            Err(ContentError::Cancelled)
        ));
        assert!(matches!(
            binary_equal(&left, &right, &cancel, 4096),
            Err(ContentError::Cancelled)
        ));
    }

    #[test]
    fn missing_file_reports_an_io_error() {
        let cancel = Cancel::new();
        assert!(matches!(
            crc32(Path::new("Z:/missing/ca-fs.bin"), &cancel, 4096),
            Err(ContentError::Io { .. })
        ));
    }

    /// An entry carrying the Unix metadata a POSIX scan records. The values are
    /// hand-built, so the test runs on every platform.
    fn posix_entry(mode: u32, uid: u32, gid: u32) -> Entry {
        let mut entry = entry("a", 1, 100);
        entry.attributes = Attributes {
            unix_mode: Some(mode),
            uid: Some(uid),
            gid: Some(gid),
            ..Attributes::default()
        };
        entry
    }

    #[test]
    fn permission_bits_are_compared_only_when_the_test_is_on() {
        let left = posix_entry(0o100_644, 1000, 1000);
        let right = posix_entry(0o100_600, 1000, 1000);

        let quiet = quick_compare(&left, &right, &QuickTests::default());
        assert!(!quiet.differences.contains(&QuickDifference::Permissions));

        let tests = QuickTests {
            unix_permissions: true,
            ..QuickTests::default()
        };
        let loud = quick_compare(&left, &right, &tests);
        assert!(loud.differences.contains(&QuickDifference::Permissions));

        let same = quick_compare(&left, &left.clone(), &tests);
        assert!(!same.differences.contains(&QuickDifference::Permissions));
    }

    #[test]
    fn the_file_type_bits_of_a_mode_never_count_as_a_permission_difference() {
        let tests = QuickTests {
            unix_permissions: true,
            ..QuickTests::default()
        };
        // Same permissions, different file type bits.
        let left = posix_entry(0o100_644, 1000, 1000);
        let right = posix_entry(0o120_644, 1000, 1000);
        let result = quick_compare(&left, &right, &tests);
        assert!(!result.differences.contains(&QuickDifference::Permissions));
    }

    #[test]
    fn owner_and_group_are_compared_only_when_their_tests_are_on() {
        let left = posix_entry(0o100_644, 1000, 1000);
        let other_owner = posix_entry(0o100_644, 1001, 1000);
        let other_group = posix_entry(0o100_644, 1000, 1001);

        let quiet = QuickTests::default();
        assert!(!quick_compare(&left, &other_owner, &quiet)
            .differences
            .contains(&QuickDifference::Owner));
        assert!(!quick_compare(&left, &other_group, &quiet)
            .differences
            .contains(&QuickDifference::Group));

        let tests = QuickTests {
            owner: true,
            group: true,
            ..QuickTests::default()
        };
        assert!(quick_compare(&left, &other_owner, &tests)
            .differences
            .contains(&QuickDifference::Owner));
        assert!(quick_compare(&left, &other_group, &tests)
            .differences
            .contains(&QuickDifference::Group));
        assert!(quick_compare(&left, &left.clone(), &tests)
            .differences
            .is_empty());
    }
}
