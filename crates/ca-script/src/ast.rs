//! The typed form of a script.
//!
//! One [`Command`] stands for one script line. [`Script`] keeps the commands in
//! file order together with the position each one started at.

use serde::{Deserialize, Serialize};

/// Position of a token in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// One based line number.
    pub line: u32,
    /// One based column number, counted in characters.
    pub column: u32,
}

impl Span {
    /// A position.
    #[must_use]
    pub fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

/// One command with the position it started at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    /// The command.
    pub command: Command,
    /// Where the command word started.
    pub span: Span,
}

/// A parsed script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    /// The commands, in file order.
    pub statements: Vec<Statement>,
}

impl Script {
    /// The commands without their positions.
    #[must_use]
    pub fn commands(&self) -> Vec<Command> {
        self.statements.iter().map(|s| s.command.clone()).collect()
    }
}

/// One side of a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// The left side.
    Left,
    /// The right side.
    Right,
}

/// A side argument that also accepts both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SideArg {
    /// The left side.
    Left,
    /// The right side.
    Right,
    /// Both sides.
    All,
}

/// Which way an operation moves data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// From the left side to the right side.
    LeftToRight,
    /// From the right side to the left side.
    RightToLeft,
}

/// The four attributes the `attrib` and `criteria` commands name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct AttrSet {
    /// Archive.
    pub archive: bool,
    /// System.
    pub system: bool,
    /// Hidden.
    pub hidden: bool,
    /// Read only.
    pub read_only: bool,
}

impl AttrSet {
    /// True when no attribute is named.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !(self.archive || self.system || self.hidden || self.read_only)
    }

    /// The letters, in the documented order.
    #[must_use]
    pub fn letters(self) -> String {
        let mut out = String::new();
        if self.archive {
            out.push('a');
        }
        if self.system {
            out.push('s');
        }
        if self.hidden {
            out.push('h');
        }
        if self.read_only {
            out.push('r');
        }
        out
    }
}

/// One `+letters` or `-letters` group of an `attrib` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttribChange {
    /// True sets the attributes, false clears them.
    pub set: bool,
    /// The attributes the group names.
    pub attrs: AttrSet,
}

/// The wider attribute set the `filter` command names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct FilterAttrSet {
    /// Archive.
    pub archive: bool,
    /// Compressed.
    pub compressed: bool,
    /// Encrypted.
    pub encrypted: bool,
    /// Hidden.
    pub hidden: bool,
    /// Content not indexed.
    pub not_indexed: bool,
    /// Symbolic link.
    pub link: bool,
    /// Offline.
    pub offline: bool,
    /// Pinned.
    pub pinned: bool,
    /// Read only.
    pub read_only: bool,
    /// System.
    pub system: bool,
    /// Temporary.
    pub temporary: bool,
    /// Unpinned.
    pub unpinned: bool,
    /// Sparse.
    pub sparse: bool,
}

impl FilterAttrSet {
    /// True when no attribute is named.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.letters().is_empty()
    }

    /// The letters, in the documented order.
    #[must_use]
    pub fn letters(self) -> String {
        let mut out = String::new();
        for (flag, letter) in [
            (self.archive, 'a'),
            (self.compressed, 'c'),
            (self.encrypted, 'e'),
            (self.hidden, 'h'),
            (self.not_indexed, 'i'),
            (self.link, 'l'),
            (self.offline, 'o'),
            (self.pinned, 'p'),
            (self.read_only, 'r'),
            (self.system, 's'),
            (self.temporary, 't'),
            (self.unpinned, 'u'),
            (self.sparse, 'z'),
        ] {
            if flag {
                out.push(letter);
            }
        }
        out
    }
}

/// The file kinds the `filter unixtype:` clause names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct UnixTypeSet {
    /// Block special file.
    pub block: bool,
    /// Character special file.
    pub character: bool,
    /// Symbolic link.
    pub link: bool,
    /// Named pipe.
    pub fifo: bool,
    /// Regular file.
    pub regular: bool,
    /// Socket.
    pub socket: bool,
}

impl UnixTypeSet {
    /// True when no kind is named.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.letters().is_empty()
    }

    /// The letters, in the documented order.
    #[must_use]
    pub fn letters(self) -> String {
        let mut out = String::new();
        for (flag, letter) in [
            (self.block, 'b'),
            (self.character, 'c'),
            (self.link, 'l'),
            (self.fifo, 'p'),
            (self.regular, 'r'),
            (self.socket, 's'),
        ] {
            if flag {
                out.push(letter);
            }
        }
        out
    }
}

/// How the `compare` command reads file contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareType {
    /// Checksum.
    Crc,
    /// Byte for byte.
    Binary,
    /// Format aware.
    RulesBased,
}

/// How the `criteria` command reads file contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentCriterion {
    /// Size only.
    Size,
    /// Checksum.
    Crc,
    /// Byte for byte.
    Binary,
    /// Format aware.
    RulesBased,
}

/// What `copyto` and `moveto` keep of a source path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathOption {
    /// Keep the shortest path that still tells the items apart.
    Relative,
    /// Keep the whole path below the base folders.
    Base,
    /// Keep no path at all.
    None,
}

/// The timestamp clause of a `criteria` command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampCriterion {
    /// Seconds two timestamps may differ by and still match.
    pub tolerance_seconds: Option<u32>,
    /// Treat a difference of exactly one hour as a match.
    pub ignore_dst: bool,
}

/// The timezone clause of a `criteria` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimezoneCriterion {
    /// Ignore a timezone difference.
    Ignore,
    /// Add an offset to one side before comparing.
    Offset {
        /// The side the offset applies to.
        side: Side,
        /// Whole hours, minus twelve through twelve.
        hours: i8,
    },
}

/// The whole criteria set a `criteria` command installs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Criteria {
    /// Attributes that take part in the comparison.
    pub attrib: Option<AttrSet>,
    /// Compare the version resource of executables.
    pub version: bool,
    /// Compare timestamps.
    pub timestamp: Option<TimestampCriterion>,
    /// How contents are compared.
    pub content: Option<ContentCriterion>,
    /// How a timezone difference is treated.
    pub timezone: Option<TimezoneCriterion>,
    /// Read what a link points at rather than the link.
    pub follow_symlinks: bool,
    /// Treat a difference the rules call unimportant as a match.
    pub ignore_unimportant: bool,
    /// Compare the owner.
    pub owner: bool,
    /// Compare the group.
    pub group: bool,
    /// Compare the access permissions.
    pub permissions: bool,
}

/// The value a date filter compares against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CutoffValue {
    /// A number of days before the current date.
    Days(u32),
    /// A date, and optionally a time, as written in the script.
    Timestamp(String),
}

/// The date clause of a `filter` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutoffSpec {
    /// True excludes items newer than the value, false excludes older ones.
    pub newer: bool,
    /// The value itself.
    pub value: CutoffValue,
}

/// The unit a size filter is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeUnit {
    /// Bytes.
    Bytes,
    /// Kilobytes.
    Kilobytes,
    /// Megabytes.
    Megabytes,
    /// Gigabytes.
    Gigabytes,
    /// Terabytes.
    Terabytes,
}

impl SizeUnit {
    /// The suffix, empty for bytes.
    #[must_use]
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Bytes => "",
            Self::Kilobytes => "KB",
            Self::Megabytes => "MB",
            Self::Gigabytes => "GB",
            Self::Terabytes => "TB",
        }
    }

    /// The number of bytes one unit stands for.
    #[must_use]
    pub fn multiplier(self) -> u64 {
        match self {
            Self::Bytes => 1,
            Self::Kilobytes => 1024,
            Self::Megabytes => 1024 * 1024,
            Self::Gigabytes => 1024 * 1024 * 1024,
            Self::Terabytes => 1024 * 1024 * 1024 * 1024,
        }
    }
}

/// The size clause of a `filter` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeSpec {
    /// True excludes items larger than the value, false excludes smaller ones.
    pub larger: bool,
    /// The number as written.
    pub value: u64,
    /// The unit the number is written in.
    pub unit: SizeUnit,
}

impl SizeSpec {
    /// The size in bytes, saturating at the widest value.
    #[must_use]
    pub fn bytes(self) -> u64 {
        self.value.saturating_mul(self.unit.multiplier())
    }
}

/// One clause of a `filter` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterClause {
    /// Name masks, separated by semicolons.
    Masks(String),
    /// A date filter, or the removal of one.
    Cutoff(Option<CutoffSpec>),
    /// A size filter, or the removal of one.
    Size(Option<SizeSpec>),
    /// An attribute filter, or the removal of one.
    Attrib(Option<FilterAttribChange>),
    /// A file kind filter, or the removal of one.
    UnixType(Option<UnixTypeChange>),
    /// Leave out items marked both system and hidden.
    ExcludeProtected,
    /// Take in items marked both system and hidden.
    IncludeProtected,
}

/// One signed group of the `filter attrib:` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterAttribChange {
    /// True keeps only items that carry the attributes, false leaves them out.
    pub include: bool,
    /// The attributes the group names.
    pub attrs: FilterAttrSet,
}

/// One signed group of the `filter unixtype:` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnixTypeChange {
    /// True keeps only the named kinds, false leaves them out.
    pub include: bool,
    /// The kinds the group names.
    pub kinds: UnixTypeSet,
}

/// What a `load` command opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadSpec {
    /// A new comparison with the built in settings.
    Default,
    /// Base folders.
    ///
    /// A single name with no second path is a base folder when it names a
    /// folder and a saved session otherwise. The choice needs the file system,
    /// so it is made when the command runs, not while it is parsed.
    Paths {
        /// The side a missing folder is created on.
        create: Option<SideArg>,
        /// The left base folder.
        left: String,
        /// The right base folder.
        right: Option<String>,
    },
}

/// How much detail the log carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogLevel {
    /// Nothing is written.
    #[default]
    None,
    /// One line for each command and each failure.
    Normal,
    /// One line for each command, each item it touched, and each failure.
    Verbose,
}

/// Where the log is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogTarget {
    /// True adds to the file, false replaces it.
    pub append: bool,
    /// The file itself.
    pub file: String,
}

/// The arguments of a `log` command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSpec {
    /// The new level, when the command names one.
    pub level: Option<LogLevel>,
    /// The new target, when the command names one.
    pub target: Option<LogTarget>,
}

/// The answer a confirmation gets while the script runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmMode {
    /// Ask. A run with no console treats this as a refusal.
    Prompt,
    /// Answer yes.
    YesToAll,
    /// Answer no.
    NoToAll,
}

/// The arguments of an `option` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionSpec {
    /// Stop the script at the first failing command.
    StopOnError,
    /// Set the answer confirmations get.
    Confirm(ConfirmMode),
}

/// How a `rename` command builds new names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenameSpec {
    /// A wildcard mask.
    Mask(String),
    /// A regular expression and the template the new name is built from.
    Regex {
        /// Expression matched against the old name.
        find: String,
        /// Template the new name is built from.
        replace: String,
    },
}

/// The comparison result a selection mask names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectResult {
    /// Items that match.
    Exact,
    /// Items that differ by something other than a timestamp.
    Diff,
    /// Items newer on the named side.
    Newer,
    /// Items older on the named side.
    Older,
    /// Items present on one side only.
    Orphan,
    /// Any result.
    All,
}

/// The item kind a selection mask names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectKind {
    /// Files only.
    Files,
    /// Folders only.
    Folders,
    /// Both.
    All,
}

/// One mask of a `select` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectMask {
    /// A side, result and kind combination.
    Mask {
        /// Side component.
        side: SideArg,
        /// Result component.
        result: SelectResult,
        /// Kind component.
        kind: SelectKind,
    },
    /// Folders that hold nothing.
    EmptyFolders,
}

/// The folder a `snapshot` command records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotSource {
    /// The left base folder.
    Left,
    /// The right base folder.
    Right,
    /// A folder named in the command.
    Path(String),
}

/// The arguments of a `snapshot` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SnapshotSpec {
    /// Record a checksum for every file.
    pub save_crc: bool,
    /// Record the version resource of executables.
    pub save_version: bool,
    /// Record the contents of archives as folders.
    pub expand_archives: bool,
    /// Record what links point at rather than the links.
    pub follow_symlinks: bool,
    /// Record folders that hold nothing.
    pub include_empty: bool,
    /// Record every file, whatever the name filters say.
    pub no_filters: bool,
    /// The folder to record.
    pub source: SnapshotSource,
    /// Where the snapshot is written.
    pub output: Option<String>,
}

/// How a `sync` command reconciles two folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMode {
    /// Copy newer items and orphans toward the target.
    Update,
    /// Make the target identical to the source.
    Mirror,
}

/// Which way a `sync` command works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncDirection {
    /// Left to right.
    LeftToRight,
    /// Right to left.
    RightToLeft,
    /// Both ways.
    All,
}

/// The arguments of a `sync` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSpec {
    /// Touch only items in expanded folders.
    pub visible: bool,
    /// Create folders on the target that hold nothing on the source.
    pub create_empty: bool,
    /// The reconciliation rule.
    pub mode: SyncMode,
    /// The direction the rule runs in.
    pub direction: SyncDirection,
}

/// The timestamp a `touch` command writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchValue {
    /// The current time.
    Now,
    /// A timestamp as written in the script.
    Timestamp(String),
}

/// The arguments of a `touch` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchSpec {
    /// Take each timestamp from the other side.
    Copy(Direction),
    /// Write one timestamp to the named side.
    Set {
        /// The side written.
        side: SideArg,
        /// The value written.
        value: TouchValue,
    },
}

/// Which report a report command writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportKind {
    /// Whatever the selected file pair's type calls for.
    File,
    /// The folder comparison.
    Folder,
    /// A byte comparison.
    Hex,
    /// A line comparison.
    Text,
    /// A table comparison. Written `data-report`.
    Data,
    /// A media comparison.
    Media,
    /// A picture comparison.
    Picture,
    /// A registry comparison.
    Registry,
    /// A version comparison.
    Version,
}

impl ReportKind {
    /// The command word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::File => "file-report",
            Self::Folder => "folder-report",
            Self::Hex => "hex-report",
            Self::Text => "text-report",
            Self::Data => "data-report",
            Self::Media => "media-report",
            Self::Picture => "picture-report",
            Self::Registry => "registry-report",
            Self::Version => "version-report",
        }
    }
}

/// The shape of a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportLayout {
    /// Two columns.
    SideBySide,
    /// Counts only.
    Summary,
    /// One column, both sides in sequence.
    Interleaved,
    /// A patch.
    Patch,
    /// A table of counts.
    Statistics,
    /// A machine readable document.
    Xml,
}

impl ReportLayout {
    /// The keyword.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::SideBySide => "side-by-side",
            Self::Summary => "summary",
            Self::Interleaved => "interleaved",
            Self::Patch => "patch",
            Self::Statistics => "statistics",
            Self::Xml => "xml",
        }
    }
}

/// One value of a report's `options:` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportOption {
    /// Treat a difference the rules call unimportant as a match.
    IgnoreUnimportant,
    /// Include every row.
    DisplayAll,
    /// Include rows that differ.
    DisplayMismatches,
    /// Include rows that match.
    DisplayMatches,
    /// Include rows that differ and the rows around them.
    DisplayContext,
    /// Leave out items present on one side only.
    DisplayNoOrphans,
    /// Include differences, without items present on one side only.
    DisplayMismatchesNoOrphans,
    /// Include items present on one side only.
    DisplayOrphans,
    /// Include items newer on the left.
    DisplayLeftNewer,
    /// Include items newer on the right.
    DisplayRightNewer,
    /// Include items newer on the left and left orphans.
    DisplayLeftNewerOrphans,
    /// Include items newer on the right and right orphans.
    DisplayRightNewerOrphans,
    /// Include items present on the left only.
    DisplayLeftOrphans,
    /// Include items present on the right only.
    DisplayRightOrphans,
    /// Write row numbers.
    LineNumbers,
    /// Cross out left difference lines.
    StrikeoutLeftDiffs,
    /// Cross out right difference lines.
    StrikeoutRightDiffs,
    /// Write the patch in the default dialect.
    PatchNormal,
    /// Write the patch with context.
    PatchContext,
    /// Write the patch in the unified dialect.
    PatchUnified,
    /// Add the version control column.
    ColumnVcs,
    /// Add the revision column.
    ColumnRevision,
    /// Add the file version column.
    ColumnVersion,
    /// Add the size column.
    ColumnSize,
    /// Add the checksum column.
    ColumnCrc,
    /// Add the timestamp column.
    ColumnTimestamp,
    /// Add the attributes column.
    ColumnAttributes,
    /// Add the owner column.
    ColumnOwner,
    /// Add the group column.
    ColumnGroup,
    /// Clear the default columns.
    ColumnNone,
    /// Write a report per file pair and link to it.
    IncludeFileLinks,
    /// Withdrawn name for the summary layout.
    StatsDescriptive,
    /// Withdrawn name for the statistics layout.
    StatsTabular,
}

impl ReportOption {
    /// The keyword.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::IgnoreUnimportant => "ignore-unimportant",
            Self::DisplayAll => "display-all",
            Self::DisplayMismatches => "display-mismatches",
            Self::DisplayMatches => "display-matches",
            Self::DisplayContext => "display-context",
            Self::DisplayNoOrphans => "display-no-orphans",
            Self::DisplayMismatchesNoOrphans => "display-mismatches-no-orphans",
            Self::DisplayOrphans => "display-orphans",
            Self::DisplayLeftNewer => "display-left-newer",
            Self::DisplayRightNewer => "display-right-newer",
            Self::DisplayLeftNewerOrphans => "display-left-newer-orphans",
            Self::DisplayRightNewerOrphans => "display-right-newer-orphans",
            Self::DisplayLeftOrphans => "display-left-orphans",
            Self::DisplayRightOrphans => "display-right-orphans",
            Self::LineNumbers => "line-numbers",
            Self::StrikeoutLeftDiffs => "strikeout-left-diffs",
            Self::StrikeoutRightDiffs => "strikeout-right-diffs",
            Self::PatchNormal => "patch-normal",
            Self::PatchContext => "patch-context",
            Self::PatchUnified => "patch-unified",
            Self::ColumnVcs => "column-vcs",
            Self::ColumnRevision => "column-revision",
            Self::ColumnVersion => "column-version",
            Self::ColumnSize => "column-size",
            Self::ColumnCrc => "column-crc",
            Self::ColumnTimestamp => "column-timestamp",
            Self::ColumnAttributes => "column-attributes",
            Self::ColumnOwner => "column-owner",
            Self::ColumnGroup => "column-group",
            Self::ColumnNone => "column-none",
            Self::IncludeFileLinks => "include-file-links",
            Self::StatsDescriptive => "stats-descriptive",
            Self::StatsTabular => "stats-tabular",
        }
    }
}

/// Where a report is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputTo {
    /// A printer.
    Printer,
    /// The clipboard.
    Clipboard,
    /// A file.
    File(String),
}

/// One value of a report's `output-options:` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputOption {
    /// Print in color.
    PrintColor,
    /// Print without color.
    PrintMono,
    /// Print upright.
    PrintPortrait,
    /// Print sideways.
    PrintLandscape,
    /// Do not wrap long lines.
    WrapNone,
    /// Wrap long lines at any character.
    WrapCharacter,
    /// Wrap long lines between words.
    WrapWord,
    /// Write HTML in color.
    HtmlColor,
    /// Write HTML without color.
    HtmlMono,
    /// Write HTML against an external stylesheet.
    HtmlCustom(String),
}

/// A comparison a report command names instead of the current selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    /// A saved session.
    Session(String),
    /// A pair of files.
    Files(String, String),
}

/// The arguments every report command takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSpec {
    /// The shape of the report.
    pub layout: ReportLayout,
    /// The `options:` list.
    pub options: Vec<ReportOption>,
    /// The heading.
    pub title: Option<String>,
    /// Where the report is sent.
    pub output_to: OutputTo,
    /// The `output-options:` list.
    pub output_options: Vec<OutputOption>,
    /// A comparison that replaces the current selection.
    pub comparison: Option<Comparison>,
}

/// What a `collapse` or `expand` command names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathsArg {
    /// Every folder.
    All,
    /// Folders named by paths relative to the base folders.
    Paths(Vec<String>),
}

/// One script command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Set or clear attributes on the selection.
    Attrib(Vec<AttribChange>),
    /// Sound the speaker.
    Beep,
    /// Close folders.
    Collapse(PathsArg),
    /// Compare the contents of the selection.
    Compare(Option<CompareType>),
    /// Copy the selection to the other side.
    Copy(Direction),
    /// Copy the selection into a folder.
    CopyTo {
        /// Which side's items are copied.
        side: SideArg,
        /// What is kept of each source path.
        path_option: PathOption,
        /// The destination folder.
        path: String,
    },
    /// Replace the comparison criteria.
    Criteria(Criteria),
    /// Remove the selection.
    Delete {
        /// Whether removed items go to the recycle bin.
        recycle_bin: Option<bool>,
        /// Which sides are removed from.
        side: SideArg,
    },
    /// Open folders.
    Expand(PathsArg),
    /// Change the filters.
    Filter(Vec<FilterClause>),
    /// Open base folders or a saved session.
    Load(LoadSpec),
    /// Change the log level or target.
    Log(LogSpec),
    /// Move the selection to the other side.
    Move(Direction),
    /// Move the selection into a folder.
    MoveTo {
        /// Which side's items are moved.
        side: SideArg,
        /// What is kept of each source path.
        path_option: PathOption,
        /// The destination folder.
        path: String,
    },
    /// Change a script processing option.
    Option(OptionSpec),
    /// Give the selection new names.
    Rename(RenameSpec),
    /// Write a report.
    Report {
        /// Which report is written.
        kind: ReportKind,
        /// The arguments.
        spec: Box<ReportSpec>,
    },
    /// Choose what later commands act on.
    Select(Vec<SelectMask>),
    /// Record a folder listing.
    Snapshot(Box<SnapshotSpec>),
    /// Reconcile the two base folders.
    Sync(SyncSpec),
    /// Write timestamps.
    Touch(TouchSpec),
}

impl Command {
    /// The command word.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Attrib(_) => "attrib",
            Self::Beep => "beep",
            Self::Collapse(_) => "collapse",
            Self::Compare(_) => "compare",
            Self::Copy(_) => "copy",
            Self::CopyTo { .. } => "copyto",
            Self::Criteria(_) => "criteria",
            Self::Delete { .. } => "delete",
            Self::Expand(_) => "expand",
            Self::Filter(_) => "filter",
            Self::Load(_) => "load",
            Self::Log(_) => "log",
            Self::Move(_) => "move",
            Self::MoveTo { .. } => "moveto",
            Self::Option(_) => "option",
            Self::Rename(_) => "rename",
            Self::Report { kind, .. } => kind.word(),
            Self::Select(_) => "select",
            Self::Snapshot(_) => "snapshot",
            Self::Sync(_) => "sync",
            Self::Touch(_) => "touch",
        }
    }
}
