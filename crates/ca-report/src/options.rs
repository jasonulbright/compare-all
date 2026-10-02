//! Layouts and options, one type per report command.
//!
//! # Forward compatibility
//!
//! Every struct carries a flattened `unknown` map and every enum carries an
//! `Unknown` arm that holds the raw value. A settings document written by a
//! later build therefore round trips through this build without loss.

use crate::palette::ReportPalette;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Fields written by another build, preserved verbatim.
pub type Unknown = BTreeMap<String, Value>;

/// Heading and side labels of one report.
///
/// `generated` is supplied by the caller, never read from the clock, so the
/// same input and options produce byte-identical output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReportMeta {
    /// Heading at the top of the report. Absent leaves the heading out.
    pub title: Option<String>,
    /// Name of the left side.
    pub left_label: String,
    /// Name of the right side.
    pub right_label: String,
    /// Time stamp text written under the heading. Absent writes none.
    pub generated: Option<String>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl ReportMeta {
    /// Build a heading with the two side labels and no time stamp.
    #[must_use]
    pub fn new(left_label: impl Into<String>, right_label: impl Into<String>) -> Self {
        Self {
            title: None,
            left_label: left_label.into(),
            right_label: right_label.into(),
            generated: None,
            unknown: Unknown::new(),
        }
    }

    /// Set the heading text.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// Which document a report writes when the layout does not decide for itself.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Plain text, with no color.
    #[default]
    PlainText,
    /// A self-contained HTML document.
    Html,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Color scheme of an HTML report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum HtmlScheme {
    /// Difference highlighting in color, from the palette.
    #[default]
    Color,
    /// No color: weight, strikeout and a leading marker separate the classes.
    Mono,
    /// An external style sheet supplies the rules.
    ///
    /// The document then carries no inline style rules and is not
    /// self-contained: it loads the named sheet.
    Custom {
        /// File name or URL of the style sheet.
        stylesheet: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: Unknown,
    },
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Color scheme of a printer report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrintScheme {
    /// No color.
    #[default]
    Mono,
    /// Difference highlighting in color.
    Color,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Page orientation of a printer report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageOrientation {
    /// Tall page.
    #[default]
    Portrait,
    /// Wide page.
    Landscape,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Treatment of a line wider than the page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wrap {
    /// The line runs past the edge and is clipped.
    #[default]
    None,
    /// The line breaks at any character. Printer output only.
    Character,
    /// The line breaks between words.
    Word,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Printer settings carried through to the page rules of the document.
///
/// This crate writes documents, never pages. The settings reach the printed
/// page through the page rules of the HTML document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrintOptions {
    /// Color scheme.
    pub scheme: PrintScheme,
    /// Page orientation.
    pub orientation: PageOrientation,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// Settings shared by every report command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputOptions {
    /// Document written when the layout does not force one.
    pub format: OutputFormat,
    /// Color scheme of an HTML document.
    pub html: HtmlScheme,
    /// Printer settings written into the page rules.
    pub print: PrintOptions,
    /// Treatment of a line wider than the page.
    pub wrap: Wrap,
    /// Colors of an HTML document.
    pub palette: ReportPalette,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl OutputOptions {
    /// Settings for a plain text document.
    #[must_use]
    pub fn plain_text() -> Self {
        Self::default()
    }

    /// Settings for an HTML document in color.
    #[must_use]
    pub fn html_color() -> Self {
        Self {
            format: OutputFormat::Html,
            html: HtmlScheme::Color,
            ..Self::default()
        }
    }

    /// Settings for an HTML document without color.
    #[must_use]
    pub fn html_mono() -> Self {
        Self {
            format: OutputFormat::Html,
            html: HtmlScheme::Mono,
            palette: ReportPalette::monochrome(),
            ..Self::default()
        }
    }

    /// Settings for an HTML document that loads an external style sheet.
    #[must_use]
    pub fn html_custom(stylesheet: impl Into<String>) -> Self {
        Self {
            format: OutputFormat::Html,
            html: HtmlScheme::Custom {
                stylesheet: stylesheet.into(),
                unknown: Unknown::new(),
            },
            ..Self::default()
        }
    }

    /// True when the settings ask for an HTML document.
    #[must_use]
    pub fn is_html(&self) -> bool {
        matches!(self.format, OutputFormat::Html)
    }

    /// The palette the document paints with, after the scheme is applied.
    #[must_use]
    pub fn effective_palette(&self) -> ReportPalette {
        match self.html {
            HtmlScheme::Mono => ReportPalette::monochrome(),
            _ => self.palette.clone(),
        }
    }
}

/// Which rows of a file comparison a report includes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayFilter {
    /// Every row.
    #[default]
    All,
    /// Only rows that differ.
    Mismatches,
    /// Only rows both sides share.
    Matches,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Which rows of a text comparison a report includes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextDisplayFilter {
    /// Every line.
    #[default]
    All,
    /// Only lines that differ.
    Mismatches,
    /// Lines that differ together with the lines around them.
    Context,
    /// Only lines both sides share.
    Matches,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Which entries of a folder comparison a report includes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderDisplayFilter {
    /// Every file.
    #[default]
    All,
    /// Only files that differ.
    Mismatches,
    /// Every file that is present on both sides.
    NoOrphans,
    /// Files that differ, excluding files present on one side only.
    MismatchesNoOrphans,
    /// Only files present on one side.
    Orphans,
    /// Only files newer on the left.
    LeftNewer,
    /// Only files newer on the right.
    RightNewer,
    /// Files newer on the left, plus files present on one side only.
    LeftNewerOrphans,
    /// Files newer on the right, plus files present on one side only.
    RightNewerOrphans,
    /// Only files present on the left alone.
    LeftOrphans,
    /// Only files present on the right alone.
    RightOrphans,
    /// Only files both sides share unchanged.
    Matches,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Layout of a text report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextLayout {
    /// Two columns, left beside right.
    #[default]
    SideBySide,
    /// One column, the two sides in turn.
    Interleaved,
    /// Counts only.
    Summary,
    /// Counts as a table of comma separated values.
    Statistics,
    /// A patch another tool can apply.
    Patch,
    /// A machine readable document.
    Xml,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Patch dialect of the patch layout.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatchFormat {
    /// Normal format: `NcM` headers with `<` and `>` bodies.
    #[default]
    Normal,
    /// Context format: `***` and `---` blocks.
    Context,
    /// Unified format: `@@` ranges with one body.
    Unified,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Layout of a folder report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderLayout {
    /// Two columns, left beside right.
    #[default]
    SideBySide,
    /// Counts and the list of differing names.
    Summary,
    /// A machine readable document.
    Xml,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Layout of a hex report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HexLayout {
    /// Two columns, left beside right.
    #[default]
    SideBySide,
    /// Counts only.
    Summary,
    /// One column, the two sides in turn.
    Interleaved,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Layout of a table report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TableLayout {
    /// Two columns, left beside right.
    #[default]
    SideBySide,
    /// Counts only.
    Summary,
    /// One column, the two sides in turn.
    Interleaved,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Layout of a report that has two layouts only.
///
/// Picture, version, media and registry reports share this pair.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PairLayout {
    /// Two columns, left beside right.
    #[default]
    SideBySide,
    /// Counts only.
    Summary,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// Settings of a text report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub struct TextReportOptions {
    /// Layout.
    pub layout: TextLayout,
    /// Which lines the report includes.
    pub display: TextDisplayFilter,
    /// Lines kept around a difference under [`TextDisplayFilter::Context`].
    pub context_lines: u32,
    /// Treat a difference that does not matter as a match.
    pub ignore_unimportant: bool,
    /// Write line numbers in the side-by-side layout.
    pub line_numbers: bool,
    /// Cross out left difference lines in the interleaved layout.
    pub strikeout_left_diffs: bool,
    /// Cross out right difference lines in the interleaved layout.
    pub strikeout_right_diffs: bool,
    /// Patch dialect of the patch layout.
    pub patch_format: PatchFormat,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// Lines kept on each side of a difference when no count is configured.
pub const DEFAULT_CONTEXT_LINES: u32 = 3;

impl TextReportOptions {
    /// Lines kept on each side of a difference, with the built-in default
    /// standing in for zero.
    #[must_use]
    pub fn resolved_context_lines(&self) -> u32 {
        if self.context_lines == 0 {
            DEFAULT_CONTEXT_LINES
        } else {
            self.context_lines
        }
    }
}

/// Columns a folder report carries beside the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub struct FolderColumns {
    /// Size in bytes. Included by default.
    pub size: bool,
    /// Modification time. Included by default.
    pub timestamp: bool,
    /// Version control status.
    pub vcs: bool,
    /// Revision.
    pub revision: bool,
    /// File version.
    pub version: bool,
    /// Cyclic redundancy check of the content.
    pub crc: bool,
    /// File system attributes.
    pub attributes: bool,
    /// Owner.
    pub owner: bool,
    /// Group.
    pub group: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl Default for FolderColumns {
    fn default() -> Self {
        Self {
            size: true,
            timestamp: true,
            vcs: false,
            revision: false,
            version: false,
            crc: false,
            attributes: false,
            owner: false,
            group: false,
            unknown: Unknown::new(),
        }
    }
}

impl FolderColumns {
    /// No columns beside the name.
    #[must_use]
    pub fn none() -> Self {
        Self {
            size: false,
            timestamp: false,
            ..Self::default()
        }
    }

    /// The selected columns in the order a report writes them.
    #[must_use]
    pub fn selected(&self) -> Vec<FolderColumn> {
        let mut columns = Vec::new();
        for (wanted, column) in [
            (self.vcs, FolderColumn::Vcs),
            (self.revision, FolderColumn::Revision),
            (self.version, FolderColumn::Version),
            (self.size, FolderColumn::Size),
            (self.crc, FolderColumn::Crc),
            (self.timestamp, FolderColumn::Timestamp),
            (self.attributes, FolderColumn::Attributes),
            (self.owner, FolderColumn::Owner),
            (self.group, FolderColumn::Group),
        ] {
            if wanted {
                columns.push(column);
            }
        }
        columns
    }
}

/// One optional folder report column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderColumn {
    /// Version control status.
    Vcs,
    /// Revision.
    Revision,
    /// File version.
    Version,
    /// Size in bytes.
    Size,
    /// Cyclic redundancy check of the content.
    Crc,
    /// Modification time.
    Timestamp,
    /// File system attributes.
    Attributes,
    /// Owner.
    Owner,
    /// Group.
    Group,
}

impl FolderColumn {
    /// Heading of the column.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Vcs => "Status",
            Self::Revision => "Revision",
            Self::Version => "Version",
            Self::Size => "Size",
            Self::Crc => "CRC",
            Self::Timestamp => "Modified",
            Self::Attributes => "Attributes",
            Self::Owner => "Owner",
            Self::Group => "Group",
        }
    }

    /// Element name the XML layout writes the column under.
    #[must_use]
    pub const fn element(self) -> &'static str {
        match self {
            Self::Vcs => "status",
            Self::Revision => "revision",
            Self::Version => "version",
            Self::Size => "size",
            Self::Crc => "crc",
            Self::Timestamp => "modified",
            Self::Attributes => "attributes",
            Self::Owner => "owner",
            Self::Group => "group",
        }
    }
}

/// Settings of a folder report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderReportOptions {
    /// Layout.
    pub layout: FolderLayout,
    /// Which entries the report includes.
    pub display: FolderDisplayFilter,
    /// Columns beside the name.
    pub columns: FolderColumns,
    /// Link each entry to a per-file report.
    ///
    /// The link is written only by a side-by-side HTML folder report; any other
    /// combination rejects the setting.
    pub include_file_links: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// Bytes one hex report row carries per side when no count is configured.
pub const DEFAULT_BYTES_PER_ROW: u32 = 16;

/// Settings of a hex report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HexReportOptions {
    /// Layout.
    pub layout: HexLayout,
    /// Which rows the report includes.
    pub display: DisplayFilter,
    /// Write byte addresses in the side-by-side layout.
    pub line_numbers: bool,
    /// Bytes one row carries per side. Zero uses the built-in count.
    pub bytes_per_row: u32,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl HexReportOptions {
    /// Bytes one row carries per side, with the built-in count standing in for
    /// zero.
    #[must_use]
    pub fn resolved_bytes_per_row(&self) -> u32 {
        if self.bytes_per_row == 0 {
            DEFAULT_BYTES_PER_ROW
        } else {
            self.bytes_per_row
        }
    }
}

/// Settings of a table report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableReportOptions {
    /// Layout.
    pub layout: TableLayout,
    /// Which rows the report includes.
    pub display: DisplayFilter,
    /// Treat a difference that does not matter as a match.
    pub ignore_unimportant: bool,
    /// Write row numbers in the side-by-side layout.
    pub line_numbers: bool,
    /// Report the current sheet alone.
    pub current_sheet_only: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// Settings of a picture report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PictureReportOptions {
    /// Layout.
    pub layout: PairLayout,
    /// Treat a difference that does not matter as a match in the summary
    /// layout.
    pub ignore_unimportant: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// Settings of a report over named records.
///
/// Version, media and registry reports share this shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordReportOptions {
    /// Layout.
    pub layout: PairLayout,
    /// Which records the report includes.
    pub display: DisplayFilter,
    /// Treat a difference that does not matter as a match.
    pub ignore_unimportant: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: Unknown,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        FolderColumn, FolderColumns, FolderDisplayFilter, HexReportOptions, HtmlScheme,
        OutputOptions, TextLayout, TextReportOptions, DEFAULT_BYTES_PER_ROW, DEFAULT_CONTEXT_LINES,
    };

    #[test]
    fn the_default_text_layout_is_side_by_side() {
        assert_eq!(TextReportOptions::default().layout, TextLayout::SideBySide);
    }

    #[test]
    fn the_default_folder_filter_includes_every_file() {
        assert_eq!(FolderDisplayFilter::default(), FolderDisplayFilter::All);
    }

    #[test]
    fn the_default_folder_columns_are_size_and_timestamp() {
        assert_eq!(
            FolderColumns::default().selected(),
            vec![FolderColumn::Size, FolderColumn::Timestamp]
        );
    }

    #[test]
    fn clearing_the_columns_leaves_none() {
        assert!(FolderColumns::none().selected().is_empty());
    }

    #[test]
    fn a_layout_name_is_kebab_case() {
        let text = serde_json::to_string(&TextLayout::SideBySide).expect("serialize");
        assert_eq!(text, "\"side-by-side\"");
    }

    #[test]
    fn an_unknown_layout_survives_a_round_trip() {
        let layout: TextLayout = serde_json::from_str("\"over-under\"").expect("deserialize");
        assert!(matches!(layout, TextLayout::Unknown(_)));
        let back = serde_json::to_string(&layout).expect("serialize");
        assert_eq!(back, "\"over-under\"");
    }

    #[test]
    fn an_unknown_scheme_object_survives_a_round_trip() {
        let scheme: HtmlScheme =
            serde_json::from_str(r#"{"futureScheme":{"weight":2}}"#).expect("deserialize");
        assert!(matches!(scheme, HtmlScheme::Unknown(_)));
        let back = serde_json::to_string(&scheme).expect("serialize");
        assert!(back.contains("futureScheme"), "{back}");
    }

    #[test]
    fn a_custom_scheme_keeps_its_stylesheet() {
        let options = OutputOptions::html_custom("report.css");
        match options.html {
            HtmlScheme::Custom { ref stylesheet, .. } => assert_eq!(stylesheet, "report.css"),
            ref other => panic!("expected a custom scheme, got {other:?}"),
        }
        assert!(options.is_html());
    }

    #[test]
    fn the_mono_scheme_paints_one_background() {
        let palette = OutputOptions::html_mono().effective_palette();
        assert_eq!(palette.difference_background, palette.background);
    }

    #[test]
    fn zero_stands_for_the_built_in_counts() {
        assert_eq!(
            TextReportOptions::default().resolved_context_lines(),
            DEFAULT_CONTEXT_LINES
        );
        assert_eq!(
            HexReportOptions::default().resolved_bytes_per_row(),
            DEFAULT_BYTES_PER_ROW
        );
    }
}
