//! What the report dialog collects and what the engine reads from it.

use super::payload::Payload;
use ca_report::options::{
    DisplayFilter, FolderDisplayFilter, FolderLayout, FolderReportOptions, HexLayout,
    HexReportOptions, OutputFormat, OutputOptions, PairLayout, PatchFormat, PictureReportOptions,
    RecordReportOptions, ReportMeta, TableLayout, TableReportOptions, TextDisplayFilter,
    TextLayout, TextReportOptions,
};
use ca_session::options::{ReportPreference, ReportTarget};
use std::path::PathBuf;

/// Width the report dialog lays its controls out in.
///
/// The smallest window the program opens at is 640 points; the dialog stays
/// inside that whatever the window is, so a wide window does not move a control
/// out of the habit of the narrow one.
pub const DIALOG_WIDTH: f32 = 560.0;

/// Why the two printer destinations take no click.
pub const PRINTER_UNAVAILABLE: &str =
    "No printer back end is built. Write an HTML file and print it from a browser.";

/// Which comparison a report is written from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportKind {
    /// Text comparison.
    Text,
    /// Folder comparison.
    Folder,
    /// Hex comparison.
    Hex,
    /// Table comparison.
    Table,
    /// Picture comparison.
    Picture,
    /// Three way merge.
    Merge,
    /// Registry comparison.
    Registry,
    /// Version resource comparison.
    Version,
    /// Media comparison.
    Media,
}

/// One layout a report kind offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutChoice {
    /// Stable name the stored settings hold the layout under.
    pub id: &'static str,
    /// What the dialog shows.
    pub label: &'static str,
    /// True when the layout decides the document itself, so the HTML and plain
    /// text choice does not apply.
    pub fixed_document: bool,
}

const fn layout(id: &'static str, label: &'static str, fixed_document: bool) -> LayoutChoice {
    LayoutChoice {
        id,
        label,
        fixed_document,
    }
}

/// One display filter a report kind offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayChoice {
    /// Stable name the stored settings hold the filter under.
    pub id: &'static str,
    /// What the dialog shows.
    pub label: &'static str,
}

const fn display(id: &'static str, label: &'static str) -> DisplayChoice {
    DisplayChoice { id, label }
}

const TEXT_LAYOUTS: &[LayoutChoice] = &[
    layout("side-by-side", "Side by side", false),
    layout("interleaved", "Interleaved", false),
    layout("summary", "Summary", false),
    layout("statistics", "Statistics, comma separated", true),
    layout("patch", "Patch", true),
    layout("xml", "XML", true),
];

const FOLDER_LAYOUTS: &[LayoutChoice] = &[
    layout("side-by-side", "Side by side", false),
    layout("summary", "Summary", false),
    layout("xml", "XML", true),
];

const PAIR_LAYOUTS: &[LayoutChoice] = &[
    layout("side-by-side", "Side by side", false),
    layout("summary", "Summary", false),
];

const INTERLEAVED_LAYOUTS: &[LayoutChoice] = &[
    layout("side-by-side", "Side by side", false),
    layout("summary", "Summary", false),
    layout("interleaved", "Interleaved", false),
];

const MERGE_LAYOUTS: &[LayoutChoice] = &[
    layout("side-by-side", "Side by side", false),
    layout("interleaved", "Interleaved", false),
    layout("summary", "Summary", false),
    layout("patch", "Patch", true),
];

const TEXT_FILTERS: &[DisplayChoice] = &[
    display("all", "Every line"),
    display("mismatches", "Lines that differ"),
    display("context", "Lines that differ, with context"),
    display("matches", "Lines that match"),
];

const ROW_FILTERS: &[DisplayChoice] = &[
    display("all", "Every row"),
    display("mismatches", "Rows that differ"),
    display("matches", "Rows that match"),
];

const FOLDER_FILTERS: &[DisplayChoice] = &[
    display("all", "Every file"),
    display("mismatches", "Files that differ"),
    display("no-orphans", "Files on both sides"),
    display("mismatches-no-orphans", "Files that differ, on both sides"),
    display("orphans", "Files on one side only"),
    display("left-newer", "Files newer on the left"),
    display("right-newer", "Files newer on the right"),
    display("left-newer-orphans", "Left newer, plus one sided files"),
    display("right-newer-orphans", "Right newer, plus one sided files"),
    display("left-orphans", "Files on the left only"),
    display("right-orphans", "Files on the right only"),
    display("matches", "Files that match"),
];

const PATCH_FORMATS: &[DisplayChoice] = &[
    display("normal", "Normal"),
    display("context", "Context"),
    display("unified", "Unified"),
];

impl ReportKind {
    /// The stable name the stored settings hold this kind under.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Folder => "folder",
            Self::Hex => "hex",
            Self::Table => "table",
            Self::Picture => "picture",
            Self::Merge => "merge",
            Self::Registry => "registry",
            Self::Version => "version",
            Self::Media => "media",
        }
    }

    /// The heading the dialog shows.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Text => "Text Compare Report",
            Self::Folder => "Folder Compare Report",
            Self::Hex => "Hex Compare Report",
            Self::Table => "Table Compare Report",
            Self::Picture => "Picture Compare Report",
            Self::Merge => "Text Merge Report",
            Self::Registry => "Registry Compare Report",
            Self::Version => "Version Compare Report",
            Self::Media => "Media Compare Report",
        }
    }

    /// The layouts this kind offers.
    #[must_use]
    pub const fn layouts(self) -> &'static [LayoutChoice] {
        match self {
            Self::Text => TEXT_LAYOUTS,
            Self::Folder => FOLDER_LAYOUTS,
            Self::Hex | Self::Table => INTERLEAVED_LAYOUTS,
            Self::Picture | Self::Registry | Self::Version | Self::Media => PAIR_LAYOUTS,
            Self::Merge => MERGE_LAYOUTS,
        }
    }

    /// The display filters this kind offers.
    #[must_use]
    pub const fn filters(self) -> &'static [DisplayChoice] {
        match self {
            Self::Text | Self::Merge => TEXT_FILTERS,
            Self::Folder => FOLDER_FILTERS,
            Self::Hex | Self::Table | Self::Registry | Self::Version | Self::Media => ROW_FILTERS,
            Self::Picture => &[],
        }
    }

    /// True when the kind carries the ignore-unimportant option.
    #[must_use]
    pub const fn has_importance(self) -> bool {
        !matches!(self, Self::Hex)
    }

    /// True when the kind carries the line number option.
    #[must_use]
    pub const fn has_line_numbers(self) -> bool {
        !matches!(
            self,
            Self::Picture | Self::Registry | Self::Version | Self::Media
        )
    }
}

/// Where a report is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// A file the user names.
    #[default]
    File,
    /// The clipboard, as text.
    Clipboard,
    /// A printer.
    Printer,
    /// A preview of the printed pages.
    PrintPreview,
}

impl Target {
    /// What the dialog shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Clipboard => "Clipboard, as text",
            Self::Printer => "Printer",
            Self::PrintPreview => "Print preview",
        }
    }

    /// True when this build can write to the destination.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::File | Self::Clipboard)
    }
}

/// Everything the dialog collects.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ReportSettings {
    /// Which comparison the report is written from.
    pub kind: ReportKind,
    /// Index into [`ReportKind::layouts`].
    pub layout: usize,
    /// Index into [`ReportKind::filters`].
    pub filter: usize,
    /// Where the report goes.
    pub target: Target,
    /// True for an HTML document, false for plain text.
    pub html: bool,
    /// Treat a difference that does not matter as a match.
    pub ignore_unimportant: bool,
    /// Write line numbers or byte addresses.
    pub line_numbers: bool,
    /// Lines kept around a difference under the context filter.
    pub context_lines: u32,
    /// Index into the patch dialects.
    pub patch: usize,
    /// Start the written file in another program.
    pub open_after_saving: bool,
    /// The file the report is written to.
    pub path: String,
    /// Heading at the top of the report.
    pub title: String,
}

impl ReportSettings {
    /// The settings a kind starts from.
    #[must_use]
    pub fn new(kind: ReportKind) -> Self {
        Self {
            kind,
            layout: 0,
            filter: 0,
            target: Target::File,
            html: true,
            ignore_unimportant: false,
            line_numbers: true,
            context_lines: 0,
            patch: 0,
            open_after_saving: false,
            path: String::new(),
            title: kind.title().to_owned(),
        }
    }

    /// The settings a kind starts from, with what it was last run under.
    #[must_use]
    pub fn from_preference(kind: ReportKind, held: Option<&ReportPreference>) -> Self {
        let mut settings = Self::new(kind);
        let Some(held) = held else {
            return settings;
        };
        settings.layout = index_of(kind.layouts().iter().map(|held| held.id), &held.layout);
        settings.filter = index_of(kind.filters().iter().map(|held| held.id), &held.display);
        settings.patch = index_of(PATCH_FORMATS.iter().map(|held| held.id), &held.patch_format);
        settings.target = match held.target {
            ReportTarget::Clipboard => Target::Clipboard,
            ReportTarget::Printer => Target::Printer,
            _ => Target::File,
        };
        if !settings.target.is_available() {
            settings.target = Target::File;
        }
        settings.html = held.format != "plain-text";
        settings.ignore_unimportant = held.ignore_unimportant;
        settings.line_numbers = held.line_numbers;
        settings.context_lines = held.context_lines;
        settings.open_after_saving = held.open_after_saving;
        if let Some(path) = &held.last_file {
            settings.path = path.to_path_buf().display().to_string();
        }
        settings
    }

    /// What the stored settings hold for the next run.
    #[must_use]
    pub fn to_preference(&self) -> ReportPreference {
        ReportPreference {
            layout: self.layout_choice().id.to_owned(),
            target: match self.target {
                Target::Clipboard => ReportTarget::Clipboard,
                Target::Printer | Target::PrintPreview => ReportTarget::Printer,
                Target::File => ReportTarget::File,
            },
            format: if self.writes_html() {
                "html"
            } else {
                "plain-text"
            }
            .to_owned(),
            display: self
                .kind
                .filters()
                .get(self.filter)
                .map_or_else(String::new, |held| held.id.to_owned()),
            patch_format: PATCH_FORMATS
                .get(self.patch)
                .map_or_else(String::new, |held| held.id.to_owned()),
            ignore_unimportant: self.ignore_unimportant,
            line_numbers: self.line_numbers,
            context_lines: self.context_lines,
            open_after_saving: self.open_after_saving,
            last_file: (!self.path.is_empty()).then(|| PathBuf::from(&self.path).into()),
            unknown: std::collections::BTreeMap::new(),
        }
    }

    /// Select the filter named `id`, where the kind offers it.
    ///
    /// A view calls this so the report starts from what the view is showing.
    pub fn select_filter(&mut self, id: &str) {
        if let Some(index) = self.kind.filters().iter().position(|held| held.id == id) {
            self.filter = index;
        }
    }

    /// The layout the settings name.
    #[must_use]
    pub fn layout_choice(&self) -> LayoutChoice {
        self.kind
            .layouts()
            .get(self.layout)
            .copied()
            .unwrap_or(TEXT_LAYOUTS[0])
    }

    /// True when the document written is HTML.
    #[must_use]
    pub fn writes_html(&self) -> bool {
        !self.layout_choice().fixed_document && self.html
    }

    /// True when the layout carries the patch dialect.
    #[must_use]
    pub fn shows_patch_format(&self) -> bool {
        self.layout_choice().id == "patch"
    }

    /// True when the filter carries a context line count.
    #[must_use]
    pub fn shows_context_lines(&self) -> bool {
        self.kind
            .filters()
            .get(self.filter)
            .is_some_and(|held| held.id == "context")
    }

    /// The suffix the written file takes.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self.layout_choice().id {
            "xml" => "xml",
            "statistics" => "csv",
            "patch" => "patch",
            _ if self.writes_html() => "html",
            _ => "txt",
        }
    }

    /// Why the report cannot be written yet, where it cannot.
    #[must_use]
    pub fn refusal(&self) -> Option<&'static str> {
        if !self.target.is_available() {
            return Some(PRINTER_UNAVAILABLE);
        }
        if self.target == Target::File && self.path.trim().is_empty() {
            return Some("Name the file the report is written to.");
        }
        None
    }

    /// The document settings the engine reads.
    #[must_use]
    pub fn output(&self) -> OutputOptions {
        if self.writes_html() {
            OutputOptions::html_color()
        } else {
            OutputOptions {
                format: OutputFormat::PlainText,
                ..OutputOptions::plain_text()
            }
        }
    }

    fn filter_id(&self) -> &'static str {
        self.kind
            .filters()
            .get(self.filter)
            .map_or("all", |held| held.id)
    }

    fn text_options(&self) -> TextReportOptions {
        TextReportOptions {
            layout: match self.layout_choice().id {
                "interleaved" => TextLayout::Interleaved,
                "summary" => TextLayout::Summary,
                "statistics" => TextLayout::Statistics,
                "patch" => TextLayout::Patch,
                "xml" => TextLayout::Xml,
                _ => TextLayout::SideBySide,
            },
            display: match self.filter_id() {
                "mismatches" => TextDisplayFilter::Mismatches,
                "context" => TextDisplayFilter::Context,
                "matches" => TextDisplayFilter::Matches,
                _ => TextDisplayFilter::All,
            },
            context_lines: self.context_lines,
            ignore_unimportant: self.ignore_unimportant,
            line_numbers: self.line_numbers,
            strikeout_left_diffs: false,
            strikeout_right_diffs: false,
            patch_format: match PATCH_FORMATS.get(self.patch).map(|held| held.id) {
                Some("context") => PatchFormat::Context,
                Some("unified") => PatchFormat::Unified,
                _ => PatchFormat::Normal,
            },
            unknown: std::collections::BTreeMap::new(),
        }
    }

    fn folder_options(&self) -> FolderReportOptions {
        FolderReportOptions {
            layout: match self.layout_choice().id {
                "summary" => FolderLayout::Summary,
                "xml" => FolderLayout::Xml,
                _ => FolderLayout::SideBySide,
            },
            display: match self.filter_id() {
                "mismatches" => FolderDisplayFilter::Mismatches,
                "no-orphans" => FolderDisplayFilter::NoOrphans,
                "mismatches-no-orphans" => FolderDisplayFilter::MismatchesNoOrphans,
                "orphans" => FolderDisplayFilter::Orphans,
                "left-newer" => FolderDisplayFilter::LeftNewer,
                "right-newer" => FolderDisplayFilter::RightNewer,
                "left-newer-orphans" => FolderDisplayFilter::LeftNewerOrphans,
                "right-newer-orphans" => FolderDisplayFilter::RightNewerOrphans,
                "left-orphans" => FolderDisplayFilter::LeftOrphans,
                "right-orphans" => FolderDisplayFilter::RightOrphans,
                "matches" => FolderDisplayFilter::Matches,
                _ => FolderDisplayFilter::All,
            },
            ..FolderReportOptions::default()
        }
    }

    fn row_filter(&self) -> DisplayFilter {
        match self.filter_id() {
            "mismatches" => DisplayFilter::Mismatches,
            "matches" => DisplayFilter::Matches,
            _ => DisplayFilter::All,
        }
    }

    fn hex_options(&self, bytes_per_row: u32) -> HexReportOptions {
        HexReportOptions {
            layout: match self.layout_choice().id {
                "summary" => HexLayout::Summary,
                "interleaved" => HexLayout::Interleaved,
                _ => HexLayout::SideBySide,
            },
            display: self.row_filter(),
            line_numbers: self.line_numbers,
            bytes_per_row,
            unknown: std::collections::BTreeMap::new(),
        }
    }

    fn table_options(&self) -> TableReportOptions {
        TableReportOptions {
            layout: match self.layout_choice().id {
                "summary" => TableLayout::Summary,
                "interleaved" => TableLayout::Interleaved,
                _ => TableLayout::SideBySide,
            },
            display: self.row_filter(),
            ignore_unimportant: self.ignore_unimportant,
            line_numbers: self.line_numbers,
            current_sheet_only: false,
            unknown: std::collections::BTreeMap::new(),
        }
    }

    fn record_options(&self) -> RecordReportOptions {
        RecordReportOptions {
            layout: if self.layout_choice().id == "summary" {
                PairLayout::Summary
            } else {
                PairLayout::SideBySide
            },
            display: self.row_filter(),
            ignore_unimportant: self.ignore_unimportant,
            unknown: std::collections::BTreeMap::new(),
        }
    }

    fn picture_options(&self) -> PictureReportOptions {
        PictureReportOptions {
            layout: if self.layout_choice().id == "summary" {
                PairLayout::Summary
            } else {
                PairLayout::SideBySide
            },
            ignore_unimportant: self.ignore_unimportant,
            unknown: std::collections::BTreeMap::new(),
        }
    }
}

/// The whole of one report run.
#[derive(Debug, Clone)]
pub struct ReportPlan {
    /// What the dialog collected.
    pub settings: ReportSettings,
    /// Heading and side labels.
    pub meta: ReportMeta,
    /// The comparison.
    pub payload: Payload,
    /// Bytes one hex row carries per side.
    pub bytes_per_row: u32,
}

impl ReportPlan {
    /// Write the document to `out`.
    ///
    /// # Errors
    /// Returns what the engine refused, or [`ca_report::ReportError::Cancelled`]
    /// once the flag is raised.
    pub fn write<W: std::io::Write + ?Sized>(
        &self,
        out: &mut W,
        cancel: &dyn ca_report::Cancel,
    ) -> ca_report::Result<()> {
        let output = self.settings.output();
        match &self.payload {
            Payload::Text(text) => ca_report::write_text_report(
                out,
                &self.meta,
                &self.settings.text_options(),
                &output,
                text.iter(),
                cancel,
            ),
            Payload::Folder(rows) => ca_report::write_folder_report(
                out,
                &self.meta,
                &self.settings.folder_options(),
                &output,
                rows.iter().cloned(),
                cancel,
            ),
            Payload::Hex(hex) => ca_report::write_hex_report(
                out,
                &self.meta,
                &self.settings.hex_options(self.bytes_per_row),
                &output,
                hex.rows(self.bytes_per_row),
                cancel,
            ),
            Payload::Table(header, rows) => ca_report::write_table_report(
                out,
                &self.meta,
                header,
                &self.settings.table_options(),
                &output,
                rows.iter().cloned(),
                cancel,
            ),
            Payload::Picture(facts) => ca_report::write_picture_report(
                out,
                &self.meta,
                &self.settings.picture_options(),
                &output,
                facts,
                cancel,
            ),
            Payload::Record(kind, rows) => ca_report::write_record_report(
                out,
                &self.meta,
                *kind,
                &self.settings.record_options(),
                &output,
                rows.iter().cloned(),
                cancel,
            ),
        }
    }
}

fn index_of<'a>(mut names: impl Iterator<Item = &'a str>, wanted: &str) -> usize {
    names.position(|held| held == wanted).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{ReportKind, ReportSettings, Target};

    #[test]
    fn every_kind_offers_at_least_two_layouts() {
        for kind in [
            ReportKind::Text,
            ReportKind::Folder,
            ReportKind::Hex,
            ReportKind::Table,
            ReportKind::Picture,
            ReportKind::Merge,
            ReportKind::Registry,
            ReportKind::Version,
            ReportKind::Media,
        ] {
            assert!(kind.layouts().len() >= 2, "{} has one layout", kind.id());
        }
    }

    #[test]
    fn a_fixed_document_layout_overrides_the_html_choice() {
        let mut settings = ReportSettings::new(ReportKind::Text);
        settings.html = true;
        settings.layout = 5;
        assert_eq!(settings.layout_choice().id, "xml");
        assert!(!settings.writes_html());
        assert_eq!(settings.extension(), "xml");
    }

    #[test]
    fn the_printer_destinations_state_why_they_refuse() {
        let mut settings = ReportSettings::new(ReportKind::Text);
        settings.target = Target::Printer;
        assert_eq!(settings.refusal(), Some(super::PRINTER_UNAVAILABLE));
        settings.target = Target::PrintPreview;
        assert!(settings.refusal().is_some());
    }

    #[test]
    fn a_file_target_wants_a_name() {
        let settings = ReportSettings::new(ReportKind::Folder);
        assert!(settings.refusal().is_some());
    }

    #[test]
    fn the_settings_survive_the_stored_form() {
        let mut settings = ReportSettings::new(ReportKind::Folder);
        settings.filter = 4;
        settings.target = Target::Clipboard;
        settings.html = false;
        settings.line_numbers = false;
        settings.path = "report.txt".to_owned();
        let back =
            ReportSettings::from_preference(ReportKind::Folder, Some(&settings.to_preference()));
        assert_eq!(back.filter, settings.filter);
        assert_eq!(back.target, Target::Clipboard);
        assert!(!back.html);
        assert!(!back.line_numbers);
        assert_eq!(back.path, "report.txt");
    }

    #[test]
    fn a_stored_printer_target_comes_back_as_a_file() {
        let mut settings = ReportSettings::new(ReportKind::Text);
        settings.target = Target::Printer;
        let back =
            ReportSettings::from_preference(ReportKind::Text, Some(&settings.to_preference()));
        assert_eq!(back.target, Target::File);
    }
}
