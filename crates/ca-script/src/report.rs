//! The report commands.
//!
//! Each command turns the current comparison into rows and hands them to the
//! report engine. A report whose engine is not built yet returns a typed
//! failure naming the command.

use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use ca_report::input::{
    folder_rows, hex_rows_iter, rows_for_hunk_iter, CellStatus, Importance, PictureFacts,
    PictureSide, RowKind, TableCell, TableHeader, TableRow,
};
use ca_report::{
    DisplayFilter, FolderColumns, FolderDisplayFilter, FolderLayout, FolderReportOptions,
    HexLayout, HexReportOptions, NeverCancel, OutputOptions, PairLayout, PatchFormat,
    PictureReportOptions, ReportMeta, TableLayout, TableReportOptions, TextDisplayFilter,
    TextLayout, TextReportOptions,
};

use crate::ast::{
    Comparison, OutputOption, OutputTo, ReportKind, ReportLayout, ReportOption, ReportSpec,
};
use crate::error::ExecError;
use crate::state::Session;

/// Lines of context a text report keeps around a difference.
const CONTEXT_LINES: u32 = 3;

/// Bytes one row of a hex report carries per side.
const HEX_ROW_BYTES: u32 = 16;

/// Maximum bytes read from each side of a text or hex report.
///
/// Both report engines retain input-derived rows while generating output, so
/// accepting arbitrarily large files can multiply memory use well beyond the
/// source size.
const MAX_DIFF_REPORT_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Write one report.
///
/// # Errors
/// Returns [`ExecError::NotSupported`] for a report or an output target with no
/// engine behind it, and [`ExecError::Io`] when the report cannot be written.
pub fn write(session: &Session, kind: ReportKind, spec: &ReportSpec) -> Result<PathBuf, ExecError> {
    let target = match &spec.output_to {
        OutputTo::File(path) => session.resolve(path),
        OutputTo::Printer => {
            return Err(ExecError::not_supported(
                kind.word(),
                "printing has no engine behind it yet",
            ))
        }
        OutputTo::Clipboard => {
            return Err(ExecError::not_supported(
                kind.word(),
                "the clipboard has no engine behind it yet",
            ))
        }
    };
    session.check_path_write(&target, "report")?;
    if session.options.dry_run {
        let mut sink = io::sink();
        render(session, kind, spec, &mut sink).map_err(|error| error.into_exec(&target))?;
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| ExecError::Io {
                context: format!("cannot create {}", parent.display()),
                source,
            })?;
        }
    }
    ca_io::replace(&target, |raw| render(session, kind, spec, raw))
        .map_err(|error| error.into_exec(&target))?;
    Ok(target)
}

/// Build one report directly into its destination writer.
fn render(
    session: &Session,
    kind: ReportKind,
    spec: &ReportSpec,
    out: &mut dyn Write,
) -> Result<(), ReportWriteError> {
    let mut out = BufWriter::new(out);
    match kind {
        ReportKind::Folder => write_folder(session, spec, &mut out)?,
        ReportKind::Text => write_text(session, kind, spec, &mut out)?,
        ReportKind::Hex => write_hex(session, spec, &mut out)?,
        ReportKind::Data => write_data(session, kind, spec, &mut out)?,
        ReportKind::Picture => write_picture(session, kind, spec, &mut out)?,
        ReportKind::Media | ReportKind::Registry | ReportKind::Version => {
            return Err(ExecError::not_supported(
                kind.word(),
                "the comparison this report reads is not built yet",
            )
            .into())
        }
        ReportKind::File => write_by_format(session, spec, &mut out)?,
    }
    out.flush().map_err(ReportWriteError::Io)?;
    Ok(())
}

enum ReportWriteError {
    Exec(ExecError),
    Io(io::Error),
}

impl From<ExecError> for ReportWriteError {
    fn from(error: ExecError) -> Self {
        Self::Exec(error)
    }
}

impl From<io::Error> for ReportWriteError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl ReportWriteError {
    fn into_exec(self, target: &Path) -> ExecError {
        match self {
            Self::Exec(error) => error,
            Self::Io(source) => ExecError::Io {
                context: format!("cannot write {}", target.display()),
                source,
            },
        }
    }
}

fn map_report_error(error: ca_report::ReportError) -> ReportWriteError {
    match error {
        ca_report::ReportError::Io(source) => ReportWriteError::Io(source),
        other => ReportWriteError::Exec(ExecError::Refused(other.to_string())),
    }
}

fn meta(session: &Session, spec: &ReportSpec, left: &Path, right: &Path) -> ReportMeta {
    let _ = session;
    let mut meta = ReportMeta::new(left.display().to_string(), right.display().to_string());
    if let Some(title) = &spec.title {
        meta = meta.with_title(title.clone());
    }
    meta
}

fn output_options(spec: &ReportSpec) -> OutputOptions {
    let mut options = OutputOptions::plain_text();
    for option in &spec.output_options {
        match option {
            OutputOption::HtmlColor => options = OutputOptions::html_color(),
            OutputOption::HtmlMono => options = OutputOptions::html_mono(),
            OutputOption::HtmlCustom(sheet) => {
                options = OutputOptions::html_custom(sheet.clone());
            }
            _ => {}
        }
    }
    for option in &spec.output_options {
        match option {
            OutputOption::WrapNone => options.wrap = ca_report::Wrap::None,
            OutputOption::WrapCharacter => options.wrap = ca_report::Wrap::Character,
            OutputOption::WrapWord => options.wrap = ca_report::Wrap::Word,
            OutputOption::PrintColor => {
                options.print.scheme = ca_report::PrintScheme::Color;
            }
            OutputOption::PrintMono => options.print.scheme = ca_report::PrintScheme::Mono,
            OutputOption::PrintPortrait => {
                options.print.orientation = ca_report::PageOrientation::Portrait;
            }
            OutputOption::PrintLandscape => {
                options.print.orientation = ca_report::PageOrientation::Landscape;
            }
            _ => {}
        }
    }
    options
}

fn write_folder(
    session: &Session,
    spec: &ReportSpec,
    out: &mut dyn Write,
) -> Result<(), ReportWriteError> {
    let (left, right) = session.bases("folder-report")?;
    let Some(tree) = &session.tree else {
        return Err(ExecError::NoComparison {
            command: "folder-report".to_string(),
        }
        .into());
    };
    let layout = match spec.layout {
        ReportLayout::SideBySide => FolderLayout::SideBySide,
        ReportLayout::Summary => FolderLayout::Summary,
        _ => FolderLayout::Xml,
    };
    let mut options = FolderReportOptions {
        layout,
        display: folder_display(spec),
        columns: folder_columns(spec),
        ..FolderReportOptions::default()
    };
    options.include_file_links = spec.options.contains(&ReportOption::IncludeFileLinks);
    if options.include_file_links {
        return Err(ExecError::not_supported(
            "folder-report",
            "a report per file pair is not built yet",
        )
        .into());
    }
    let rows = folder_rows(tree);
    ca_report::write_folder_report(
        out,
        &meta(session, spec, left, right),
        &options,
        &output_options(spec),
        rows,
        &NeverCancel,
    )
    .map_err(map_report_error)
}

fn folder_columns(spec: &ReportSpec) -> FolderColumns {
    let named = spec.options.iter().any(|option| {
        matches!(
            option,
            ReportOption::ColumnVcs
                | ReportOption::ColumnRevision
                | ReportOption::ColumnVersion
                | ReportOption::ColumnSize
                | ReportOption::ColumnCrc
                | ReportOption::ColumnTimestamp
                | ReportOption::ColumnAttributes
                | ReportOption::ColumnOwner
                | ReportOption::ColumnGroup
                | ReportOption::ColumnNone
        )
    });
    if !named {
        return FolderColumns::default();
    }
    let mut columns = FolderColumns {
        size: false,
        timestamp: false,
        ..FolderColumns::default()
    };
    for option in &spec.options {
        match option {
            ReportOption::ColumnVcs => columns.vcs = true,
            ReportOption::ColumnRevision => columns.revision = true,
            ReportOption::ColumnVersion => columns.version = true,
            ReportOption::ColumnSize => columns.size = true,
            ReportOption::ColumnCrc => columns.crc = true,
            ReportOption::ColumnTimestamp => columns.timestamp = true,
            ReportOption::ColumnAttributes => columns.attributes = true,
            ReportOption::ColumnOwner => columns.owner = true,
            ReportOption::ColumnGroup => columns.group = true,
            _ => {}
        }
    }
    columns
}

fn folder_display(spec: &ReportSpec) -> FolderDisplayFilter {
    for option in &spec.options {
        let value = match option {
            ReportOption::DisplayMismatches => FolderDisplayFilter::Mismatches,
            ReportOption::DisplayMatches => FolderDisplayFilter::Matches,
            ReportOption::DisplayNoOrphans => FolderDisplayFilter::NoOrphans,
            ReportOption::DisplayMismatchesNoOrphans => FolderDisplayFilter::MismatchesNoOrphans,
            ReportOption::DisplayOrphans => FolderDisplayFilter::Orphans,
            ReportOption::DisplayLeftNewer => FolderDisplayFilter::LeftNewer,
            ReportOption::DisplayRightNewer => FolderDisplayFilter::RightNewer,
            ReportOption::DisplayLeftNewerOrphans => FolderDisplayFilter::LeftNewerOrphans,
            ReportOption::DisplayRightNewerOrphans => FolderDisplayFilter::RightNewerOrphans,
            ReportOption::DisplayLeftOrphans => FolderDisplayFilter::LeftOrphans,
            ReportOption::DisplayRightOrphans => FolderDisplayFilter::RightOrphans,
            _ => continue,
        };
        return value;
    }
    FolderDisplayFilter::All
}

/// One file a file level report reads.
struct ReportFile {
    /// The path the report names.
    shown: PathBuf,
    /// The source that holds the file and the file's path inside it, for a
    /// side that is not a local folder.
    inside: Option<(ca_fs::Source, ca_vfs::VfsPath)>,
}

impl ReportFile {
    fn on_disk(path: PathBuf) -> Self {
        Self {
            shown: path,
            inside: None,
        }
    }

    /// One side of a selected pair, read through the source of that side.
    fn listed(source: ca_fs::Source, base: &Path, entry: &ca_fs::Entry) -> Result<Self, ExecError> {
        let shown = base.join(&entry.rel);
        if source.is_local_folder() {
            return Ok(Self::on_disk(shown));
        }
        let path = source
            .entry_path(&entry.rel)
            .map_err(|error| ExecError::Refused(format!("{}: {error}", shown.display())))?;
        Ok(Self {
            shown,
            inside: Some((source, path)),
        })
    }

    fn read_limited(&self, limit: usize) -> Result<Vec<u8>, ExecError> {
        let Some((source, path)) = &self.inside else {
            let size = std::fs::metadata(&self.shown)
                .map_err(|source| ExecError::Io {
                    context: format!("cannot inspect {}", self.shown.display()),
                    source,
                })?
                .len();
            return read_limited_from(&self.shown, Some(size), limit, || {
                std::fs::File::open(&self.shown)
            });
        };

        read_limited_from(&self.shown, None, limit, || {
            source
                .file_system()
                .open(path, &ca_vfs::Cancel::new())
                .map_err(|error| io::Error::other(error.to_string()))
        })
    }
}

fn read_limited_from<R: io::Read>(
    shown: &Path,
    declared_size: Option<u64>,
    limit: usize,
    open: impl FnOnce() -> io::Result<R>,
) -> Result<Vec<u8>, ExecError> {
    if declared_size.is_some_and(|size| size > u64::try_from(limit).unwrap_or(u64::MAX)) {
        return Err(ExecError::Refused(format!(
            "{} exceeds the {limit} byte report input limit",
            shown.display()
        )));
    }
    let reader = open().map_err(|source| ExecError::Io {
        context: format!("cannot read {}", shown.display()),
        source,
    })?;
    let mut bytes = Vec::new();
    reader
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| ExecError::Io {
            context: format!("cannot read {}", shown.display()),
            source,
        })?;
    if bytes.len() > limit {
        return Err(ExecError::Refused(format!(
            "{} exceeds the {limit} byte report input limit",
            shown.display()
        )));
    }
    Ok(bytes)
}

/// The two files a file level report reads.
fn pair(
    session: &Session,
    kind: ReportKind,
    spec: &ReportSpec,
) -> Result<(ReportFile, ReportFile), ExecError> {
    match &spec.comparison {
        Some(Comparison::Files(left, right)) => Ok((
            ReportFile::on_disk(session.resolve(left)),
            ReportFile::on_disk(session.resolve(right)),
        )),
        Some(Comparison::Session(_)) => Err(ExecError::not_supported(
            kind.word(),
            "saved sessions are not built yet",
        )),
        None => {
            let (left_base, right_base) = session.bases(kind.word())?;
            let (Some(tree), Some(left_source), Some(right_source)) = (
                session.tree.as_ref(),
                session.source(ca_fs::Side::Left),
                session.source(ca_fs::Side::Right),
            ) else {
                return Err(ExecError::NoComparison {
                    command: kind.word().to_string(),
                });
            };
            let mut chosen: Vec<(&ca_fs::Entry, &ca_fs::Entry)> = Vec::new();
            tree.walk(&mut |node| {
                if node.is_dir || !session.selection.contains(&node.rel) {
                    return;
                }
                if let (Some(left), Some(right)) = (node.left.as_ref(), node.right.as_ref()) {
                    chosen.push((left, right));
                }
            });
            match chosen.as_slice() {
                [] => Err(ExecError::NoSelection {
                    command: kind.word().to_string(),
                }),
                [(left, right)] => Ok((
                    ReportFile::listed(left_source, left_base, left)?,
                    ReportFile::listed(right_source, right_base, right)?,
                )),
                _ => Err(ExecError::Refused(format!(
                    "{} reads one pair of files; the selection holds more",
                    kind.word()
                ))),
            }
        }
    }
}

fn write_text(
    session: &Session,
    kind: ReportKind,
    spec: &ReportSpec,
    out: &mut dyn Write,
) -> Result<(), ReportWriteError> {
    let (left_file, right_file) = pair(session, kind, spec)?;
    let left_text = read_text(&left_file)?;
    let right_text = read_text(&right_file)?;
    let left_lines = ca_diff::split_lines(&left_text);
    let right_lines = ca_diff::split_lines(&right_text);
    let hunks = ca_diff::diff_line_slices(
        &left_lines,
        &right_lines,
        &ca_diff::LineCompareOptions::default(),
    );
    let rows = hunks.iter().flat_map(|hunk| {
        let importance = match hunk.kind {
            ca_diff::HunkKind::Same => None,
            _ => Some(ca_diff::Importance::Important),
        };
        rows_for_hunk_iter(hunk, &left_lines, &right_lines, importance)
    });
    let options = TextReportOptions {
        layout: text_layout(spec.layout),
        display: text_display(spec),
        context_lines: CONTEXT_LINES,
        ignore_unimportant: spec.options.contains(&ReportOption::IgnoreUnimportant),
        line_numbers: spec.options.contains(&ReportOption::LineNumbers),
        strikeout_left_diffs: spec.options.contains(&ReportOption::StrikeoutLeftDiffs),
        strikeout_right_diffs: spec.options.contains(&ReportOption::StrikeoutRightDiffs),
        patch_format: patch_format(spec),
        ..TextReportOptions::default()
    };
    ca_report::write_text_report(
        out,
        &meta(session, spec, &left_file.shown, &right_file.shown),
        &options,
        &output_options(spec),
        rows,
        &NeverCancel,
    )
    .map_err(map_report_error)
}

fn text_layout(layout: ReportLayout) -> TextLayout {
    match layout {
        ReportLayout::SideBySide => TextLayout::SideBySide,
        ReportLayout::Summary => TextLayout::Summary,
        ReportLayout::Interleaved => TextLayout::Interleaved,
        ReportLayout::Patch => TextLayout::Patch,
        ReportLayout::Statistics => TextLayout::Statistics,
        ReportLayout::Xml => TextLayout::Xml,
    }
}

fn text_display(spec: &ReportSpec) -> TextDisplayFilter {
    for option in &spec.options {
        let value = match option {
            ReportOption::DisplayMismatches => TextDisplayFilter::Mismatches,
            ReportOption::DisplayMatches => TextDisplayFilter::Matches,
            ReportOption::DisplayContext => TextDisplayFilter::Context,
            _ => continue,
        };
        return value;
    }
    TextDisplayFilter::All
}

fn patch_format(spec: &ReportSpec) -> PatchFormat {
    for option in &spec.options {
        let value = match option {
            ReportOption::PatchContext => PatchFormat::Context,
            ReportOption::PatchUnified => PatchFormat::Unified,
            ReportOption::PatchNormal => PatchFormat::Normal,
            _ => continue,
        };
        return value;
    }
    PatchFormat::Normal
}

fn write_hex(
    session: &Session,
    spec: &ReportSpec,
    out: &mut dyn Write,
) -> Result<(), ReportWriteError> {
    let (left_file, right_file) = pair(session, ReportKind::Hex, spec)?;
    let left = read_diff_report_bytes(&left_file)?;
    let right = read_diff_report_bytes(&right_file)?;
    let hunks = ca_diff::diff_bytes(&left, &right, ca_diff::ByteAlignment::default());
    let rows = hex_rows_iter(&hunks, &left, &right, HEX_ROW_BYTES);
    let options = HexReportOptions {
        layout: match spec.layout {
            ReportLayout::Summary => HexLayout::Summary,
            ReportLayout::Interleaved => HexLayout::Interleaved,
            _ => HexLayout::SideBySide,
        },
        display: hex_display(spec),
        line_numbers: spec.options.contains(&ReportOption::LineNumbers),
        bytes_per_row: HEX_ROW_BYTES,
        ..HexReportOptions::default()
    };
    ca_report::write_hex_report(
        out,
        &meta(session, spec, &left_file.shown, &right_file.shown),
        &options,
        &output_options(spec),
        rows,
        &NeverCancel,
    )
    .map_err(map_report_error)
}

fn hex_display(spec: &ReportSpec) -> DisplayFilter {
    for option in &spec.options {
        let value = match option {
            ReportOption::DisplayMismatches => DisplayFilter::Mismatches,
            ReportOption::DisplayMatches => DisplayFilter::Matches,
            _ => continue,
        };
        return value;
    }
    DisplayFilter::All
}

/// Pick the report a file level report writes from the format of the left
/// file, and write it.
fn write_by_format(
    session: &Session,
    spec: &ReportSpec,
    out: &mut dyn Write,
) -> Result<(), ReportWriteError> {
    let (left, _) = pair(session, ReportKind::File, spec)?;
    let name = left
        .shown
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let registry = ca_grammar::builtin::registry_with_media();
    let format = registry.lookup(&name);
    match format.kind.known() {
        Some(ca_grammar::FormatKind::Table) => write_data(session, ReportKind::File, spec, out),
        Some(ca_grammar::FormatKind::Picture) => {
            write_picture(session, ReportKind::File, spec, out)
        }
        Some(ca_grammar::FormatKind::Hex) => write_hex(session, spec, out),
        _ => write_text(session, ReportKind::File, spec, out),
    }
}

// -- table report ------------------------------------------------------------

/// Read one side of a table report, with the field syntax taken from the data.
fn read_table(file: &ReportFile) -> Result<ca_table::Table, ExecError> {
    let input_limit = ca_table::parse::MAX_INPUT_BYTES;
    let bytes = file.read_limited(input_limit)?;
    let text = ca_text::decode(&bytes, &ca_text::DecodeOptions::default()).text;
    if text.len() > input_limit {
        return Err(ExecError::Refused(format!(
            "{} exceeds the {input_limit} byte table report input limit after decoding",
            file.shown.display()
        )));
    }
    let format = ca_table::detect_format(&text, &ca_table::DetectOptions::default());
    let options = ca_table::ParseOptions {
        syntax: format.syntax,
        first_line_contains: format.first_line_contains,
        unknown: ca_table::Unknown::new(),
    };
    ca_table::parse(&text, &options).map_err(|error| {
        ExecError::Refused(format!(
            "cannot read {} as a table: {error}",
            file.shown.display()
        ))
    })
}

fn table_layout(layout: ReportLayout) -> TableLayout {
    match layout {
        ReportLayout::Summary => TableLayout::Summary,
        ReportLayout::Interleaved => TableLayout::Interleaved,
        _ => TableLayout::SideBySide,
    }
}

fn write_data(
    session: &Session,
    kind: ReportKind,
    spec: &ReportSpec,
    out: &mut dyn Write,
) -> Result<(), ReportWriteError> {
    let (left_file, right_file) = pair(session, kind, spec)?;
    let left = read_table(&left_file)?;
    let right = read_table(&right_file)?;
    let settings = ca_table::SchemaSettings::default();
    let schema = ca_table::Schema::build(&left, &right, &settings);
    let alignment = ca_table::align::align(
        &left,
        &right,
        &schema,
        &ca_table::RowAlignOptions::default(),
    )
    .map_err(|error| ExecError::Refused(error.to_string()))?;
    let comparison = ca_table::compare::compare(&left, &right, &schema, &alignment)
        .map_err(|error| ExecError::Refused(error.to_string()))?;

    let header = TableHeader {
        sheet: None,
        columns: schema
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
    };
    let rows = table_rows(&left, &right, &schema, &comparison);
    let options = TableReportOptions {
        layout: table_layout(spec.layout),
        display: hex_display(spec),
        ignore_unimportant: spec.options.contains(&ReportOption::IgnoreUnimportant),
        line_numbers: spec.options.contains(&ReportOption::LineNumbers),
        ..TableReportOptions::default()
    };
    ca_report::write_table_report(
        out,
        &meta(session, spec, &left_file.shown, &right_file.shown),
        &header,
        &options,
        &output_options(spec),
        rows,
        &NeverCancel,
    )
    .map_err(map_report_error)
}

/// Turn one comparison into the report's rows.
fn table_rows(
    left: &ca_table::Table,
    right: &ca_table::Table,
    schema: &ca_table::Schema,
    comparison: &ca_table::TableComparison,
) -> Vec<TableRow> {
    let mut rows = Vec::with_capacity(comparison.rows.len());
    for (index, row) in comparison.rows.iter().enumerate() {
        let mut cells = Vec::with_capacity(schema.columns.len());
        for (column, mapping) in schema.columns.iter().enumerate() {
            let status = comparison
                .cell(index, column)
                .map_or(CellStatus::Same, CellStatus::from);
            cells.push(TableCell {
                status,
                left: cell_text(left, row.pair.left, mapping.left),
                right: cell_text(right, row.pair.right, mapping.right),
            });
        }
        rows.push(TableRow {
            left_number: row.pair.left.map(|value| u64::from(value) + 1),
            right_number: row.pair.right.map(|value| u64::from(value) + 1),
            kind: RowKind::from(row.status),
            importance: match row.status {
                ca_table::RowStatus::Same => None,
                ca_table::RowStatus::Unimportant => Some(Importance::Unimportant),
                _ => Some(Importance::Important),
            },
            cells,
        });
    }
    rows
}

fn cell_text(table: &ca_table::Table, row: Option<u32>, column: Option<u32>) -> String {
    let (Some(row), Some(column)) = (row, column) else {
        return String::new();
    };
    let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) else {
        return String::new();
    };
    table.cell_text(row, column).into_owned()
}

// -- picture report ----------------------------------------------------------

fn picture_layout(layout: ReportLayout) -> PairLayout {
    match layout {
        ReportLayout::Summary => PairLayout::Summary,
        _ => PairLayout::SideBySide,
    }
}

fn write_picture(
    session: &Session,
    kind: ReportKind,
    spec: &ReportSpec,
    out: &mut dyn Write,
) -> Result<(), ReportWriteError> {
    let (left_file, right_file) = pair(session, kind, spec)?;
    let left = decode_picture(&left_file)?;
    let right = decode_picture(&right_file)?;
    let ignore_unimportant = spec.options.contains(&ReportOption::IgnoreUnimportant);
    let compare_options = ca_image::CompareOptions {
        ignore_unimportant,
        ..ca_image::CompareOptions::default()
    };
    let result = ca_image::compare(
        &left.image,
        &right.image,
        &compare_options,
        &ca_image::NeverCancel,
    )
    .map_err(|error| ExecError::Refused(error.to_string()))?;
    let facts = PictureFacts {
        left: picture_side(&left),
        right: picture_side(&right),
        totals: result.totals.into(),
        tolerance: compare_options.tolerance,
        // The report embeds a difference picture only when the caller encodes
        // one; a script writes the counts and the two sides' facts.
        difference_png: None,
    };
    let options = PictureReportOptions {
        layout: picture_layout(spec.layout),
        ignore_unimportant,
        ..PictureReportOptions::default()
    };
    ca_report::write_picture_report(
        out,
        &meta(session, spec, &left_file.shown, &right_file.shown),
        &options,
        &output_options(spec),
        &facts,
        &NeverCancel,
    )
    .map_err(map_report_error)
}

fn decode_picture(file: &ReportFile) -> Result<ca_image::DecodedImage, ExecError> {
    let mut options = ca_image::DecodeOptions::default();
    let io_error = |source| ExecError::Io {
        context: format!("cannot read {}", file.shown.display()),
        source,
    };
    let decoded = if let Some((source, path)) = &file.inside {
        let open = source
            .file_system()
            .open(path, &ca_vfs::Cancel::new())
            .map_err(|error| io_error(io::Error::other(error.to_string())))?;
        if open.is_seekable() {
            ca_image::decode_reader(BufReader::new(SeekableReportFile(open)), &options)
        } else {
            let bytes = read_picture_stream(open, &file.shown, options.limits.max_decoded_bytes)?;
            // The encoded input remains alive during decoding, so reserve its
            // allocated capacity from the decoder's peak buffer budget.
            options.limits.max_decoded_bytes = options
                .limits
                .max_decoded_bytes
                .saturating_sub(u64::try_from(bytes.capacity()).unwrap_or(u64::MAX));
            ca_image::decode_bytes(&bytes, &options)
        }
    } else {
        let input = std::fs::File::open(&file.shown).map_err(io_error)?;
        ca_image::decode_reader(BufReader::new(input), &options)
    };
    decoded.map_err(|error| {
        ExecError::Refused(format!(
            "cannot read {} as a picture: {error}",
            file.shown.display()
        ))
    })
}

struct SeekableReportFile(ca_vfs::OpenFile);

impl Read for SeekableReportFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}

impl Seek for SeekableReportFile {
    fn seek(&mut self, at: SeekFrom) -> io::Result<u64> {
        self.0
            .seek(at)
            .map_err(|error| io::Error::other(error.to_string()))
    }
}

fn read_picture_stream(
    mut input: ca_vfs::OpenFile,
    shown: &Path,
    budget: u64,
) -> Result<Vec<u8>, ExecError> {
    let limit = usize::try_from(budget / 2).unwrap_or(usize::MAX / 2);
    let refused = || {
        ExecError::Refused(format!(
            "{} exceeds the {limit} byte buffered picture report input limit",
            shown.display()
        ))
    };
    if input.len_hint().is_some_and(|size| size > budget / 2) {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let request = chunk
            .len()
            .min(limit.saturating_sub(bytes.len()).saturating_add(1));
        let count = input
            .read(&mut chunk[..request])
            .map_err(|source| ExecError::Io {
                context: format!("cannot read {}", shown.display()),
                source,
            })?;
        if count == 0 {
            return Ok(bytes);
        }
        let needed = bytes.len().saturating_add(count);
        if needed > limit {
            return Err(refused());
        }
        if needed > bytes.capacity() {
            let capacity = needed.max(bytes.capacity().saturating_mul(2)).min(limit);
            bytes
                .try_reserve_exact(capacity.saturating_sub(bytes.len()))
                .map_err(|error| {
                    ExecError::Refused(format!("cannot buffer {}: {error}", shown.display()))
                })?;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

fn picture_side(decoded: &ca_image::DecodedImage) -> PictureSide {
    PictureSide {
        width: decoded.metadata.width,
        height: decoded.metadata.height,
        format: decoded.metadata.format.name().to_owned(),
        ..PictureSide::default()
    }
    .with_fidelity(decoded.metadata.fidelity)
}

/// Read a file as text, with the encoding taken from its bytes.
fn read_text(file: &ReportFile) -> Result<String, ExecError> {
    let bytes = read_diff_report_bytes(file)?;
    Ok(ca_text::decode(&bytes, &ca_text::DecodeOptions::default()).text)
}

fn read_diff_report_bytes(file: &ReportFile) -> Result<Vec<u8>, ExecError> {
    file.read_limited(MAX_DIFF_REPORT_INPUT_BYTES)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{read_limited_from, ExecError, ReportFile};
    use std::io::Cursor;
    use std::path::Path;

    struct MeteredPicture {
        read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    struct MeteredReader {
        input: Cursor<Vec<u8>>,
        read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl std::io::Read for MeteredReader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let count = self.input.read(bytes)?;
            self.read
                .fetch_add(count, std::sync::atomic::Ordering::SeqCst);
            Ok(count)
        }
    }

    impl std::io::Seek for MeteredReader {
        fn seek(&mut self, at: std::io::SeekFrom) -> std::io::Result<u64> {
            self.input.seek(at)
        }
    }

    impl ca_vfs::FileSystem for MeteredPicture {
        fn root_label(&self) -> String {
            "picture".to_owned()
        }
        fn capabilities(&self) -> ca_vfs::Capabilities {
            ca_vfs::LocalFs::new(".").capabilities()
        }
        fn list(
            &self,
            _: &ca_vfs::VfsPath,
            _: &ca_vfs::Cancel,
        ) -> ca_vfs::VfsResult<Vec<ca_vfs::VfsEntry>> {
            Err(ca_vfs::VfsError::unsupported("list"))
        }
        fn metadata(&self, _: &ca_vfs::VfsPath) -> ca_vfs::VfsResult<ca_vfs::VfsEntry> {
            Err(ca_vfs::VfsError::unsupported("metadata"))
        }
        fn open(
            &self,
            _: &ca_vfs::VfsPath,
            _: &ca_vfs::Cancel,
        ) -> ca_vfs::VfsResult<ca_vfs::OpenFile> {
            Ok(ca_vfs::OpenFile::seekable(
                MeteredReader {
                    input: Cursor::new(vec![0; 4 * 1024 * 1024]),
                    read: std::sync::Arc::clone(&self.read),
                },
                Some(4 * 1024 * 1024),
            ))
        }
    }

    #[test]
    fn picture_reports_check_the_header_without_reading_the_entire_seekable_input() {
        let read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = ca_fs::Source::over(
            ca_fs::SourceKind::Archive,
            std::sync::Arc::new(MeteredPicture {
                read: std::sync::Arc::clone(&read),
            }),
        );
        let file = ReportFile {
            shown: "picture.bin".into(),
            inside: Some((source, ca_vfs::VfsPath::root())),
        };
        assert!(matches!(
            super::decode_picture(&file),
            Err(ExecError::Refused(_))
        ));
        assert!(read.load(std::sync::atomic::Ordering::SeqCst) <= 16 * 1024);
    }

    #[test]
    fn buffered_picture_reports_stop_at_the_budget_without_a_size_hint() {
        let read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let input = ca_vfs::OpenFile::streaming(
            MeteredReader {
                input: Cursor::new(vec![0; 1024]),
                read: std::sync::Arc::clone(&read),
            },
            None,
        );
        assert!(matches!(
            super::read_picture_stream(input, Path::new("picture.bin"), 32),
            Err(ExecError::Refused(_))
        ));
        assert_eq!(read.load(std::sync::atomic::Ordering::SeqCst), 17);
        let input = ca_vfs::OpenFile::streaming(Cursor::new(vec![0; 12]), None);
        let bytes = super::read_picture_stream(input, Path::new("picture.bin"), 32).unwrap();
        assert_eq!(bytes.len(), 12);
        assert!(bytes.capacity() <= 16);
    }

    #[test]
    fn an_oversized_report_is_rejected_before_opening_and_growth_is_bounded() {
        let path = Path::new("oversized.csv");
        let mut opened = false;
        let result = read_limited_from(path, Some(5), 4, || {
            opened = true;
            Ok(Cursor::new(Vec::new()))
        });
        assert!(matches!(result, Err(ExecError::Refused(_))));
        assert!(
            !opened,
            "a declared oversized input must be refused before open"
        );

        let result = read_limited_from(path, None, 4, || Ok(Cursor::new(b"12345".to_vec())));
        assert!(matches!(result, Err(ExecError::Refused(_))));

        let file = tempfile::NamedTempFile::new().unwrap();
        let input_limit = u64::try_from(ca_table::parse::MAX_INPUT_BYTES).unwrap();
        file.as_file().set_len(input_limit + 1).unwrap();
        let report_file = ReportFile::on_disk(file.path().to_path_buf());
        assert!(matches!(
            report_file.read_limited(ca_table::parse::MAX_INPUT_BYTES),
            Err(ExecError::Refused(_))
        ));
    }

    #[test]
    fn text_and_hex_reports_refuse_large_inputs_before_reading_them() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file()
            .set_len(u64::try_from(super::MAX_DIFF_REPORT_INPUT_BYTES + 1).unwrap())
            .unwrap();
        let report_file = ReportFile::on_disk(file.path().to_path_buf());

        assert!(matches!(
            super::read_diff_report_bytes(&report_file),
            Err(ExecError::Refused(_))
        ));
        assert!(matches!(
            super::read_text(&report_file),
            Err(ExecError::Refused(_))
        ));
    }
}
