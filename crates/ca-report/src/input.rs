//! Report-side input types.
//!
//! A report reads plain data, never an engine's working state. The types here
//! are the whole input surface, so an engine can change its own shape without
//! changing a report. `From` conversions bridge the engine types where the
//! bridge is a field copy.
//!
//! Every report takes its rows as an iterator. A caller that already holds a
//! whole comparison converts it with the helpers below; a caller that reads a
//! large file feeds rows as it produces them, and the report never holds more
//! than the rows one block needs.

use std::time::{SystemTime, UNIX_EPOCH};

/// How one row of a comparison is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowKind {
    /// Both sides hold the same content.
    #[default]
    Same,
    /// Both sides hold content and it differs.
    Changed,
    /// Only the left side holds content.
    LeftOnly,
    /// Only the right side holds content.
    RightOnly,
}

impl RowKind {
    /// True for every kind except [`RowKind::Same`].
    #[must_use]
    pub const fn is_difference(self) -> bool {
        !matches!(self, Self::Same)
    }

    /// Single letter written in the plain text and monochrome layouts.
    #[must_use]
    pub const fn marker(self) -> char {
        match self {
            Self::Same => ' ',
            Self::Changed => '*',
            Self::LeftOnly => '<',
            Self::RightOnly => '>',
        }
    }
}

impl From<ca_diff::HunkKind> for RowKind {
    fn from(kind: ca_diff::HunkKind) -> Self {
        match kind {
            ca_diff::HunkKind::Same => Self::Same,
            ca_diff::HunkKind::Changed => Self::Changed,
            ca_diff::HunkKind::LeftOnly => Self::LeftOnly,
            ca_diff::HunkKind::RightOnly => Self::RightOnly,
        }
    }
}

/// Whether a difference counts against the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Importance {
    /// The difference counts.
    Important,
    /// The difference is filtered out by the ignore-unimportant option.
    Unimportant,
}

impl From<ca_diff::Importance> for Importance {
    fn from(importance: ca_diff::Importance) -> Self {
        match importance {
            ca_diff::Importance::Important => Self::Important,
            ca_diff::Importance::Unimportant => Self::Unimportant,
        }
    }
}

/// A byte range inside one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// First byte of the span.
    pub start: u32,
    /// One past the last byte of the span.
    pub end: u32,
}

impl From<ca_diff::Span> for Span {
    fn from(span: ca_diff::Span) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }
}

/// One side of one text row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextCell {
    /// One-based line number.
    pub number: u64,
    /// Line content without its terminator.
    pub text: String,
    /// Byte ranges of the line that the other side does not hold.
    pub spans: Vec<Span>,
}

impl TextCell {
    /// Build a cell from a line number and its text, with no inline spans.
    #[must_use]
    pub fn new(number: u64, text: impl Into<String>) -> Self {
        Self {
            number,
            text: text.into(),
            spans: Vec::new(),
        }
    }

    /// Split the line into runs, marking the runs the other side lacks.
    ///
    /// A span that falls outside the line, or that does not land on a character
    /// boundary, is dropped rather than panicking: the spans come from a
    /// separate engine pass and the two can disagree about a line.
    #[must_use]
    pub fn runs(&self) -> Vec<(bool, &str)> {
        let mut runs = Vec::new();
        let mut cursor = 0usize;
        for span in &self.spans {
            let start = span.start as usize;
            let end = span.end as usize;
            if start >= end || end > self.text.len() || start < cursor {
                continue;
            }
            if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
                continue;
            }
            if cursor < start {
                runs.push((false, &self.text[cursor..start]));
            }
            runs.push((true, &self.text[start..end]));
            cursor = end;
        }
        if cursor < self.text.len() {
            runs.push((false, &self.text[cursor..]));
        }
        runs
    }
}

/// One aligned row of a text comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextRow {
    /// Classification of the row.
    pub kind: RowKind,
    /// Importance of the row. `None` on a row both sides share.
    pub importance: Option<Importance>,
    /// Left side, absent on a right-only row.
    pub left: Option<TextCell>,
    /// Right side, absent on a left-only row.
    pub right: Option<TextCell>,
}

impl TextRow {
    /// True when the row is a difference the options do not filter out.
    #[must_use]
    pub fn counts_as_difference(&self, ignore_unimportant: bool) -> bool {
        if !self.kind.is_difference() {
            return false;
        }
        !(ignore_unimportant && self.importance == Some(Importance::Unimportant))
    }
}

/// Build the rows of one classified hunk.
///
/// The two slices hold the whole side, and the hunk names the range it covers,
/// so the caller can walk hunks one at a time without copying either side.
#[must_use]
pub fn rows_of_hunk(
    hunk: &ca_diff::ClassifiedHunk,
    left_lines: &[&str],
    right_lines: &[&str],
) -> Vec<TextRow> {
    rows_of_hunk_iter(hunk, left_lines, right_lines).collect()
}

/// Build the rows of one classified hunk lazily.
///
/// This lets report callers turn a large hunk into one row at a time instead
/// of holding every row in memory before writing the report.
pub fn rows_of_hunk_iter<'a>(
    hunk: &'a ca_diff::ClassifiedHunk,
    left_lines: &'a [&'a str],
    right_lines: &'a [&'a str],
) -> impl Iterator<Item = TextRow> + 'a {
    rows_from_ranges_iter(
        &hunk.hunk,
        left_lines,
        right_lines,
        &hunk.left_lines,
        &hunk.right_lines,
        hunk.importance,
    )
}

/// Build rows for an unclassified diff hunk lazily, assigning one importance
/// to every difference row.
pub fn rows_for_hunk_iter<'a>(
    hunk: &'a ca_diff::Hunk,
    left_lines: &'a [&'a str],
    right_lines: &'a [&'a str],
    importance: Option<ca_diff::Importance>,
) -> impl Iterator<Item = TextRow> + 'a {
    rows_from_ranges_iter(hunk, left_lines, right_lines, &[], &[], importance)
}

fn rows_from_ranges_iter<'a>(
    hunk: &'a ca_diff::Hunk,
    left_lines: &'a [&'a str],
    right_lines: &'a [&'a str],
    left_importance: &'a [ca_diff::Importance],
    right_importance: &'a [ca_diff::Importance],
    default_importance: Option<ca_diff::Importance>,
) -> impl Iterator<Item = TextRow> + 'a {
    let kind = RowKind::from(hunk.kind);
    let left_range = hunk.left.clone();
    let right_range = hunk.right.clone();
    let left_count = left_range.len();
    let right_count = right_range.len();
    let rows = left_count.max(right_count);
    (0..rows).map(move |index| {
        let left = left_range
            .start
            .checked_add(u32::try_from(index).unwrap_or(u32::MAX))
            .filter(|line| *line < left_range.end)
            .map(|line| {
                TextCell::new(
                    u64::from(line) + 1,
                    left_lines.get(line as usize).copied().unwrap_or_default(),
                )
            });
        let right = right_range
            .start
            .checked_add(u32::try_from(index).unwrap_or(u32::MAX))
            .filter(|line| *line < right_range.end)
            .map(|line| {
                TextCell::new(
                    u64::from(line) + 1,
                    right_lines.get(line as usize).copied().unwrap_or_default(),
                )
            });
        let importance = left_importance
            .get(index)
            .or_else(|| right_importance.get(index))
            .copied()
            .or(default_importance)
            .map(Importance::from);
        let row_kind = match (left.is_some(), right.is_some()) {
            (true, false) if kind != RowKind::Same => RowKind::LeftOnly,
            (false, true) if kind != RowKind::Same => RowKind::RightOnly,
            _ => kind,
        };
        TextRow {
            kind: row_kind,
            importance: if row_kind.is_difference() {
                importance
            } else {
                None
            },
            left,
            right,
        }
    })
}

/// Status of one folder entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryStatus {
    /// No comparison has run.
    #[default]
    NotCompared,
    /// The two sides match.
    Same,
    /// The two sides differ, with no side newer.
    Different,
    /// The two sides differ and the left side is newer.
    LeftNewer,
    /// The two sides differ and the right side is newer.
    RightNewer,
    /// The entry is present on the left only.
    LeftOrphan,
    /// The entry is present on the right only.
    RightOrphan,
    /// The name is a file on one side and a folder on the other.
    KindMismatch,
    /// The entry could not be compared.
    Error,
}

impl EntryStatus {
    /// True for the two orphan statuses.
    #[must_use]
    pub const fn is_orphan(self) -> bool {
        matches!(self, Self::LeftOrphan | Self::RightOrphan)
    }

    /// True for every status that counts as a mismatch.
    #[must_use]
    pub const fn is_difference(self) -> bool {
        matches!(
            self,
            Self::Different
                | Self::LeftNewer
                | Self::RightNewer
                | Self::LeftOrphan
                | Self::RightOrphan
                | Self::KindMismatch
        )
    }

    /// Word written in the status column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotCompared => "Not compared",
            Self::Same => "Same",
            Self::Different => "Different",
            Self::LeftNewer => "Left newer",
            Self::RightNewer => "Right newer",
            Self::LeftOrphan => "Left only",
            Self::RightOrphan => "Right only",
            Self::KindMismatch => "Kind mismatch",
            Self::Error => "Error",
        }
    }

    /// Name written in the XML layout.
    #[must_use]
    pub const fn element_value(self) -> &'static str {
        match self {
            Self::NotCompared => "not-compared",
            Self::Same => "same",
            Self::Different => "different",
            Self::LeftNewer => "left-newer",
            Self::RightNewer => "right-newer",
            Self::LeftOrphan => "left-orphan",
            Self::RightOrphan => "right-orphan",
            Self::KindMismatch => "kind-mismatch",
            Self::Error => "error",
        }
    }
}

impl From<ca_fs::compare::NodeStatus> for EntryStatus {
    fn from(status: ca_fs::compare::NodeStatus) -> Self {
        use ca_fs::compare::NodeStatus as Node;
        match status {
            Node::NotCompared => Self::NotCompared,
            Node::Same => Self::Same,
            Node::Different => Self::Different,
            Node::LeftNewer => Self::LeftNewer,
            Node::RightNewer => Self::RightNewer,
            Node::LeftOrphan => Self::LeftOrphan,
            Node::RightOrphan => Self::RightOrphan,
            Node::KindMismatch => Self::KindMismatch,
            Node::Error => Self::Error,
        }
    }
}

/// What one side of a folder entry holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SideFacts {
    /// False when the entry is missing from this side.
    pub present: bool,
    /// Size in bytes.
    pub size: Option<u64>,
    /// Modification time, already formatted by the caller.
    pub timestamp: Option<String>,
    /// Cyclic redundancy check of the content.
    pub crc: Option<String>,
    /// File version.
    pub version: Option<String>,
    /// Revision.
    pub revision: Option<String>,
    /// Version control status.
    pub vcs: Option<String>,
    /// File system attributes.
    pub attributes: Option<String>,
    /// Owner.
    pub owner: Option<String>,
    /// Group.
    pub group: Option<String>,
}

impl SideFacts {
    /// A side the comparison found nothing on.
    #[must_use]
    pub fn absent() -> Self {
        Self::default()
    }
}

impl From<&ca_fs::scan::Entry> for SideFacts {
    fn from(entry: &ca_fs::scan::Entry) -> Self {
        Self {
            present: true,
            size: if entry.is_dir { None } else { Some(entry.size) },
            timestamp: entry.modified.map(format_timestamp),
            crc: None,
            version: None,
            revision: None,
            vcs: None,
            attributes: Some(format_attributes(&entry.attributes)),
            owner: entry.attributes.uid.map(|uid| uid.to_string()),
            group: entry.attributes.gid.map(|gid| gid.to_string()),
        }
    }
}

/// One entry of a folder comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderRow {
    /// Depth below the two base folders. The base folders themselves are zero.
    pub depth: u32,
    /// Final path component.
    pub name: String,
    /// Path below the base folders, with forward slashes.
    pub relative_path: String,
    /// True for a folder.
    pub is_dir: bool,
    /// What the comparison found.
    pub status: EntryStatus,
    /// Left side.
    pub left: SideFacts,
    /// Right side.
    pub right: SideFacts,
    /// Relative address of a per-file report for this entry.
    ///
    /// The caller writes the per-file report and names it here. A folder
    /// report links to the address only when the link option is on.
    pub link: Option<String>,
}

/// Flatten a compared tree into report rows, deepest last within each folder.
///
/// The whole tree is already in memory when this is called, so the result is a
/// vector. A caller that walks a tree it streams builds [`FolderRow`] values
/// itself and never holds the tree.
#[must_use]
pub fn folder_rows(root: &ca_fs::compare::Node) -> Vec<FolderRow> {
    let mut rows = Vec::new();
    push_folder_rows(root, 0, &mut rows, true);
    rows
}

fn push_folder_rows(
    node: &ca_fs::compare::Node,
    depth: u32,
    rows: &mut Vec<FolderRow>,
    is_root: bool,
) {
    if !is_root {
        rows.push(FolderRow {
            depth,
            name: node.name.clone(),
            relative_path: node.rel.to_string_lossy().replace('\\', "/"),
            is_dir: node.is_dir,
            status: EntryStatus::from(node.status),
            left: node
                .left
                .as_ref()
                .map_or_else(SideFacts::absent, Into::into),
            right: node
                .right
                .as_ref()
                .map_or_else(SideFacts::absent, Into::into),
            link: None,
        });
    }
    let child_depth = if is_root { 0 } else { depth + 1 };
    for child in &node.children {
        push_folder_rows(child, child_depth, rows, false);
    }
}

/// Render a file system attribute set as the letters a report column carries.
#[must_use]
pub fn format_attributes(attributes: &ca_fs::scan::Attributes) -> String {
    let mut text = String::new();
    for (flag, letter) in [
        (attributes.read_only, 'R'),
        (attributes.hidden, 'H'),
        (attributes.system, 'S'),
        (attributes.archive, 'A'),
    ] {
        if flag {
            text.push(letter);
        }
    }
    text
}

/// Render a time as `YYYY-MM-DD HH:MM:SS` in coordinated universal time.
///
/// The report never reads the clock, and the rendering never reads a locale, so
/// two runs over the same comparison write the same bytes.
#[must_use]
pub fn format_timestamp(time: SystemTime) -> String {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => i64::try_from(delta.as_secs()).unwrap_or(i64::MAX),
        Err(back) => -i64::try_from(back.duration().as_secs()).unwrap_or(i64::MAX),
    };
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    let second = time_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Convert a count of days since 1970-01-01 into a civil date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * shifted_month + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    })
    .unwrap_or(1);
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// What a table comparison found in one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellStatus {
    /// The two cells hold the same value.
    #[default]
    Same,
    /// The two cells differ in a way that matters.
    Different,
    /// The two cells differ in a way that does not matter.
    Unimportant,
    /// The cell is present on the left only.
    LeftOnly,
    /// The cell is present on the right only.
    RightOnly,
}

impl From<ca_table::compare::CellStatus> for CellStatus {
    fn from(status: ca_table::compare::CellStatus) -> Self {
        use ca_table::compare::CellStatus as Cell;
        match status {
            Cell::Different => Self::Different,
            Cell::Unimportant => Self::Unimportant,
            Cell::LeftOnly => Self::LeftOnly,
            Cell::RightOnly => Self::RightOnly,
            _ => Self::Same,
        }
    }
}

/// One cell of a table report row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableCell {
    /// What the comparison found.
    pub status: CellStatus,
    /// Left value.
    pub left: String,
    /// Right value.
    pub right: String,
}

/// One aligned row of a table comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableRow {
    /// One-based left row number, absent on a right-only row.
    pub left_number: Option<u64>,
    /// One-based right row number, absent on a left-only row.
    pub right_number: Option<u64>,
    /// Rolled up classification.
    pub kind: RowKind,
    /// Importance of the row. `None` on a row both sides share.
    pub importance: Option<Importance>,
    /// Cells in comparison column order.
    pub cells: Vec<TableCell>,
}

/// Column names and sheet name of a table comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableHeader {
    /// Name of the sheet the rows come from, when the format has sheets.
    pub sheet: Option<String>,
    /// Comparison column names, in order.
    pub columns: Vec<String>,
}

impl From<&ca_table::schema::Schema> for TableHeader {
    fn from(schema: &ca_table::schema::Schema) -> Self {
        Self {
            sheet: None,
            columns: schema
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        }
    }
}

impl From<ca_table::compare::RowStatus> for RowKind {
    fn from(status: ca_table::compare::RowStatus) -> Self {
        use ca_table::compare::RowStatus as Row;
        match status {
            Row::Same => Self::Same,
            Row::LeftOnly => Self::LeftOnly,
            Row::RightOnly => Self::RightOnly,
            _ => Self::Changed,
        }
    }
}

/// One row of a hex comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HexRow {
    /// Classification of the row.
    pub kind: RowKind,
    /// Address of the first left byte, absent on a right-only row.
    pub left_offset: Option<u64>,
    /// Address of the first right byte, absent on a left-only row.
    pub right_offset: Option<u64>,
    /// Left bytes.
    pub left: Vec<u8>,
    /// Right bytes.
    pub right: Vec<u8>,
}

/// Cut the byte hunks of two buffers into rows of a fixed width.
///
/// The two buffers stay borrowed; the result holds one row's bytes at a time
/// only because the caller asked for a vector.
#[must_use]
pub fn hex_rows(
    hunks: &[ca_diff::ByteHunk],
    left: &[u8],
    right: &[u8],
    bytes_per_row: u32,
) -> Vec<HexRow> {
    hex_rows_iter(hunks, left, right, bytes_per_row).collect()
}

/// Cut byte hunks into rows lazily, retaining only the row being consumed.
pub fn hex_rows_iter<'a>(
    hunks: &'a [ca_diff::ByteHunk],
    left: &'a [u8],
    right: &'a [u8],
    bytes_per_row: u32,
) -> impl Iterator<Item = HexRow> + 'a {
    let width = u64::from(bytes_per_row.max(1));
    std::iter::from_fn({
        let mut hunk_index = 0usize;
        let mut offset = 0u64;
        move || loop {
            let hunk = hunks.get(hunk_index)?;
            let left_len = hunk.left.end.saturating_sub(hunk.left.start);
            let right_len = hunk.right.end.saturating_sub(hunk.right.start);
            let span = left_len.max(right_len);
            if offset >= span {
                hunk_index += 1;
                offset = 0;
                continue;
            }
            let take = width.min(span - offset);
            let left_start = hunk.left.start.saturating_add(offset);
            let right_start = hunk.right.start.saturating_add(offset);
            let left_slice = slice_of(left, left_start, take, hunk.left.end);
            let right_slice = slice_of(right, right_start, take, hunk.right.end);
            let row = HexRow {
                kind: RowKind::from(hunk.kind),
                left_offset: (!left_slice.is_empty()).then_some(left_start),
                right_offset: (!right_slice.is_empty()).then_some(right_start),
                left: left_slice,
                right: right_slice,
            };
            offset = offset.saturating_add(take);
            return Some(row);
        }
    })
}

fn slice_of(buffer: &[u8], start: u64, len: u64, limit: u64) -> Vec<u8> {
    if start >= limit {
        return Vec::new();
    }
    let end = (start + len).min(limit);
    let start = usize::try_from(start).unwrap_or(usize::MAX);
    let end = usize::try_from(end).unwrap_or(usize::MAX);
    buffer
        .get(start..end.min(buffer.len()))
        .unwrap_or(&[])
        .to_vec()
}

/// Pixel counts of a picture comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PixelTotals {
    /// Pixels that match.
    pub same: u64,
    /// Pixels that differ inside the tolerance.
    pub similar: u64,
    /// Pixels that differ beyond the tolerance.
    pub different: u64,
    /// Pixels the left picture alone covers.
    pub left_only: u64,
    /// Pixels the right picture alone covers.
    pub right_only: u64,
}

impl PixelTotals {
    /// Pixels the comparison looked at.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.same
            .saturating_add(self.similar)
            .saturating_add(self.different)
            .saturating_add(self.left_only)
            .saturating_add(self.right_only)
    }

    /// Pixels that count as a difference.
    #[must_use]
    pub const fn difference_count(self, ignore_unimportant: bool) -> u64 {
        let base = self
            .different
            .saturating_add(self.left_only)
            .saturating_add(self.right_only);
        if ignore_unimportant {
            base
        } else {
            base.saturating_add(self.similar)
        }
    }
}

impl From<ca_image::compare::Totals> for PixelTotals {
    fn from(totals: ca_image::compare::Totals) -> Self {
        Self {
            same: totals.same,
            similar: totals.similar,
            different: totals.different,
            left_only: totals.left_only,
            right_only: totals.right_only,
        }
    }
}

/// What one picture carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PictureSide {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Format name, as the decoder reports it.
    pub format: String,
    /// Bits one channel of a stored sample occupies. Zero when unreported.
    pub bits_per_channel: u8,
    /// True when the file stores wider samples than the comparison buffer.
    pub precision_reduced: bool,
    /// True when the file stores cyan, magenta, yellow and black samples.
    pub cmyk: bool,
    /// True when the file carries a color profile.
    pub icc_profile: bool,
    /// Metadata the report lists, name first.
    pub metadata: Vec<(String, String)>,
}

impl PictureSide {
    /// Copy the decoder's fidelity record into the side.
    #[must_use]
    pub fn with_fidelity(mut self, fidelity: ca_image::decode::Fidelity) -> Self {
        self.bits_per_channel = fidelity.bits_per_channel;
        self.precision_reduced = fidelity.precision_reduced;
        self.cmyk = fidelity.cmyk;
        self.icc_profile = fidelity.icc_profile;
        self
    }
}

/// The whole input of a picture report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PictureFacts {
    /// Left picture.
    pub left: PictureSide,
    /// Right picture.
    pub right: PictureSide,
    /// Pixel counts.
    pub totals: PixelTotals,
    /// Tolerance the comparison ran under.
    pub tolerance: u8,
    /// The difference picture, already encoded as a portable network graphic.
    ///
    /// An HTML report embeds the bytes; any other format names the size only.
    /// This crate encodes nothing, so a caller that wants no picture passes
    /// `None` and the report costs no memory for one.
    pub difference_png: Option<Vec<u8>>,
}

/// One record of a comparison over named values.
///
/// Version, media and registry comparisons all produce a list of named values,
/// so one row type serves all three reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordRow {
    /// Name of the value.
    pub name: String,
    /// Group the value belongs to, such as a key or a stream.
    pub group: Option<String>,
    /// Classification.
    pub kind: RowKind,
    /// Importance. `None` on a record both sides share.
    pub importance: Option<Importance>,
    /// Left value.
    pub left: Option<String>,
    /// Right value.
    pub right: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        civil_from_days, format_timestamp, hex_rows, hex_rows_iter, rows_for_hunk_iter,
        rows_of_hunk, EntryStatus, RowKind, Span, TextCell,
    };
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn a_hunk_kind_maps_onto_a_row_kind() {
        assert_eq!(RowKind::from(ca_diff::HunkKind::Changed), RowKind::Changed);
        assert_eq!(RowKind::from(ca_diff::HunkKind::Same), RowKind::Same);
    }

    #[test]
    fn a_node_status_maps_onto_an_entry_status() {
        assert_eq!(
            EntryStatus::from(ca_fs::compare::NodeStatus::LeftOrphan),
            EntryStatus::LeftOrphan
        );
        assert!(EntryStatus::LeftOrphan.is_orphan());
        assert!(EntryStatus::LeftNewer.is_difference());
        assert!(!EntryStatus::Same.is_difference());
    }

    #[test]
    fn a_changed_hunk_pairs_its_lines() {
        let hunk = ca_diff::ClassifiedHunk {
            hunk: ca_diff::Hunk {
                kind: ca_diff::HunkKind::Changed,
                left: 0..2,
                right: 0..1,
            },
            importance: Some(ca_diff::Importance::Important),
            left_lines: vec![ca_diff::Importance::Important; 2],
            right_lines: vec![ca_diff::Importance::Important],
        };
        let rows = rows_of_hunk(&hunk, &["a", "b"], &["A"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, RowKind::Changed);
        assert_eq!(rows[1].kind, RowKind::LeftOnly);
        assert_eq!(rows[0].left.as_ref().expect("left").number, 1);
        assert_eq!(rows[1].right, None);
    }

    #[test]
    fn text_rows_can_be_consumed_from_a_very_large_hunk_incrementally() {
        let hunk = ca_diff::Hunk {
            kind: ca_diff::HunkKind::Changed,
            left: 0..u32::MAX,
            right: 0..u32::MAX,
        };
        let rows = rows_for_hunk_iter(&hunk, &[], &[], Some(ca_diff::Importance::Important))
            .take(3)
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].left.as_ref().expect("first left line").number, 1);
        assert_eq!(rows[2].right.as_ref().expect("third right line").number, 3);
    }

    #[test]
    fn inline_runs_split_a_line() {
        let cell = TextCell {
            number: 1,
            text: "hello world".into(),
            spans: vec![Span { start: 6, end: 11 }],
        };
        assert_eq!(cell.runs(), vec![(false, "hello "), (true, "world")]);
    }

    #[test]
    fn a_span_past_the_end_of_the_line_is_dropped() {
        let cell = TextCell {
            number: 1,
            text: "ab".into(),
            spans: vec![Span { start: 1, end: 40 }],
        };
        assert_eq!(cell.runs(), vec![(false, "ab")]);
    }

    #[test]
    fn a_span_inside_a_character_is_dropped() {
        let cell = TextCell {
            number: 1,
            text: "é".into(),
            spans: vec![Span { start: 0, end: 1 }],
        };
        assert_eq!(cell.runs(), vec![(false, "é")]);
    }

    #[test]
    fn byte_hunks_cut_into_rows_of_the_requested_width() {
        let hunks = vec![ca_diff::ByteHunk {
            kind: ca_diff::HunkKind::Changed,
            left: 0..5,
            right: 0..5,
        }];
        let rows = hex_rows(&hunks, &[1, 2, 3, 4, 5], &[9, 8, 7, 6, 5], 2);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].left, vec![1, 2]);
        assert_eq!(rows[2].left, vec![5]);
        assert_eq!(rows[2].left_offset, Some(4));
    }

    #[test]
    fn hex_rows_can_be_consumed_from_a_very_large_hunk_incrementally() {
        let hunk = ca_diff::ByteHunk {
            kind: ca_diff::HunkKind::Changed,
            left: 0..u64::MAX,
            right: 0..u64::MAX,
        };
        let rows = hex_rows_iter(&[hunk], &[], &[], 16)
            .take(3)
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.kind == RowKind::Changed));
    }

    #[test]
    fn a_left_only_hunk_leaves_the_right_row_empty() {
        let hunks = vec![ca_diff::ByteHunk {
            kind: ca_diff::HunkKind::LeftOnly,
            left: 0..3,
            right: 0..0,
        }];
        let rows = hex_rows(&hunks, &[1, 2, 3], &[], 16);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].right.is_empty());
        assert_eq!(rows[0].right_offset, None);
    }

    #[test]
    fn the_epoch_renders_as_its_civil_date() {
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01 00:00:00");
    }

    #[test]
    fn a_known_instant_renders_in_universal_time() {
        let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(format_timestamp(time), "2023-11-14 22:13:20");
    }

    #[test]
    fn a_leap_day_renders_correctly() {
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn a_time_before_the_epoch_renders() {
        let time = UNIX_EPOCH - Duration::from_secs(86_400);
        assert_eq!(format_timestamp(time), "1969-12-31 00:00:00");
    }
}
