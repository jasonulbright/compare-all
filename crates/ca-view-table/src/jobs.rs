//! The background work behind a table comparison.
//!
//! Reading the two files, decoding them, deciding their field syntax, parsing
//! them into tables, building the schema, pairing the rows and classifying the
//! cells all run on one worker thread. The frame thread only paints what the
//! worker posted.
//!
//! Two entry points exist because a settings change does not need the files
//! read again. [`spawn_load`] runs every stage; [`spawn_recompare`] starts from
//! tables the view already holds.

use ca_table::align::{align_cancellable, Alignment, RowAlignOptions};
use ca_table::compare::{compare_with, CompareOptions, TableComparison};
use ca_table::detect::{detect, DetectOptions};
use ca_table::parse::{parse, ParseOptions, ParseWarning, Table};
use ca_table::schema::{Schema, SchemaSettings};
use ca_table::TableError;
use ca_text::{DecodeOptions, LoadedText, TextBuffer};
use ca_ui::save::{FileSystem, RealFileSystem, Stamp};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use calamine::Reader;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One step of the pipeline, in the order it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Reading the two files and decoding their bytes.
    Reading,
    /// Deciding the field syntax of each side.
    Detecting,
    /// Splitting the decoded text into rows of cells.
    Parsing,
    /// Pairing the columns of the two sides.
    Schema,
    /// Pairing the rows of the two sides.
    Aligning,
    /// Classifying the cells of each pair.
    Comparing,
}

impl Stage {
    /// What the status bar says while the step runs.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reading => "Reading files",
            Self::Detecting => "Detecting format",
            Self::Parsing => "Parsing",
            Self::Schema => "Mapping columns",
            Self::Aligning => "Aligning rows",
            Self::Comparing => "Comparing",
        }
    }
}

/// How one side is read into a table.
#[derive(Debug, Clone, Default)]
pub struct FormatSettings {
    /// Fixed parse options, or `None` to read the syntax from the data.
    pub parse: Option<ParseOptions>,
    /// Encoding the side is decoded with, or `None` to detect it.
    pub encoding: Option<ca_text::TextEncoding>,
}

/// Everything a comparison needs beyond the two paths.
#[derive(Debug, Clone, Default)]
pub struct TableSettings {
    /// How worksheets on the two sides are paired.
    pub sheets: ca_session::settings::table::TablePairing,
    /// How the left file is read.
    pub left_format: FormatSettings,
    /// How the right file is read.
    pub right_format: FormatSettings,
    /// Controls over the detection sample.
    pub detect: DetectOptions,
    /// Column pairing and per-column handling.
    pub schema: SchemaSettings,
    /// Row pairing.
    pub align: RowAlignOptions,
    /// Cell classification.
    pub compare: CompareOptions,
}

/// What one side turned out to be.
#[derive(Debug, Clone, Default)]
pub struct SideFacts {
    /// Label of the encoding the bytes were decoded with.
    pub encoding: String,
    /// True when decoding omits or replaces malformed input.
    pub had_errors: bool,
    /// Short description of the field syntax that was used.
    pub syntax: String,
    /// What line one was read as.
    pub header: String,
    /// What parsing recovered from.
    pub warnings: Vec<ParseWarning>,
    /// The parse options the table was read with. An edited text is parsed
    /// again with exactly these, so an edit never changes how the file splits.
    pub options: ParseOptions,
    /// What the file looked like when it was read, for the changed-on-disk
    /// check of a save.
    pub stamp: Option<Stamp>,
    /// The encoding, byte order mark and trailer to write the text back with.
    /// Its buffer is empty; a save puts the text in.
    pub template: Option<Arc<LoadedText>>,
}

/// One side of a finished comparison.
#[derive(Debug, Clone)]
pub struct Side {
    /// The parsed table.
    pub table: Arc<Table>,
    /// What reading the file turned out to be.
    pub facts: SideFacts,
}

/// A finished comparison.
#[derive(Debug, Clone)]
pub struct TableData {
    /// Left side.
    pub left: Side,
    /// Right side.
    pub right: Side,
    /// The comparison columns.
    pub schema: Arc<Schema>,
    /// The row pairing.
    pub alignment: Arc<Alignment>,
    /// The cell classification.
    pub comparison: Arc<TableComparison>,
    /// The decoded text of each side, when this comparison read the files.
    /// The view edits this text; a comparison that did not read the files
    /// carries none, and the view keeps the text it holds.
    pub texts: Option<(Arc<String>, Arc<String>)>,
    /// The paired sheet inputs available to the view.
    pub sheet_pairs: Vec<SheetPair>,
    /// The selected sheet in `sheet_pairs`.
    pub active_sheet: usize,
    /// Whether this input came from a multi-sheet-capable format.
    pub show_sheet_tabs: bool,
    /// Left-side sheet names in source order.
    pub left_sheet_names: Vec<String>,
    /// Right-side sheet names in source order.
    pub right_sheet_names: Vec<String>,
}

/// One pair of sheet inputs, including an empty side when a sheet is unpaired.
#[derive(Debug, Clone)]
pub struct SheetPair {
    /// What the bottom tab calls this pair.
    pub label: String,
    /// The left-side sheet name, if one exists.
    pub left_name: Option<String>,
    /// The right-side sheet name, if one exists.
    pub right_name: Option<String>,
    /// Left sheet index in source order, if one exists.
    pub left_index: Option<u32>,
    /// Right sheet index in source order, if one exists.
    pub right_index: Option<u32>,
    /// The left-side table, or an empty table for a right-only sheet.
    pub left: Side,
    /// The right-side table, or an empty table for a left-only sheet.
    pub right: Side,
    /// Text snapshots for ordinary editable text tables.
    pub texts: Option<(Arc<String>, Arc<String>)>,
}

#[derive(Debug, Default)]
struct SheetContext {
    pairs: Vec<SheetPair>,
    active: usize,
    show_tabs: bool,
    names: (Vec<String>, Vec<String>),
}

/// What the worker posts back.
#[derive(Debug)]
pub enum TableMessage {
    /// A step of the pipeline has begun.
    Progress(Stage),
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
    /// The comparison is ready.
    Ready(Box<TableData>),
}

impl Terminal for TableMessage {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            TableMessage::Failed(_) | TableMessage::Cancelled | TableMessage::Ready(_)
        )
    }

    fn cancelled() -> Self {
        TableMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        TableMessage::Failed(detail)
    }
}

/// Read both files and compare them on a worker thread.
#[must_use]
pub fn spawn_load(
    left: PathBuf,
    right: PathBuf,
    settings: TableSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<TableMessage> {
    spawn_load_with_clipboard(left, right, settings, [None, None], notify)
}

/// Read the sides, using captured text for clipboard inputs where present.
///
/// Clipboard contents stay in memory; no file is created for them.
#[must_use]
pub fn spawn_load_with_clipboard(
    left: PathBuf,
    right: PathBuf,
    settings: TableSettings,
    clipboard: [Option<String>; 2],
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<TableMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            load_and_compare(&left, &right, &settings, &clipboard, emitter, cancel);
        },
        notify,
    )
}

/// Parse edited text again and compare.
///
/// A side with text is parsed from that text with the options it was first
/// read with; a side without is compared as it stands.
#[must_use]
pub fn spawn_reparse(
    left: (Side, Option<Arc<String>>),
    right: (Side, Option<Arc<String>>),
    settings: TableSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<TableMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            emitter.send(TableMessage::Progress(Stage::Parsing));
            let left = match reparse(left.0, left.1) {
                Ok(side) => side,
                Err(message) => {
                    emitter.send(TableMessage::Failed(message));
                    return;
                }
            };
            let right = match reparse(right.0, right.1) {
                Ok(side) => side,
                Err(message) => {
                    emitter.send(TableMessage::Failed(message));
                    return;
                }
            };
            if cancel.is_cancelled() {
                return;
            }
            compare_tables(
                left,
                right,
                &settings,
                emitter,
                cancel,
                None,
                SheetContext::default(),
            );
        },
        notify,
    )
}

fn reparse(side: Side, text: Option<Arc<String>>) -> Result<Side, String> {
    let Some(text) = text else {
        return Ok(side);
    };
    let table = parse(&text, &side.facts.options).map_err(|error| error.to_string())?;
    let mut facts = side.facts;
    facts.header = header_label(&table);
    facts.warnings = table.warnings().to_vec();
    Ok(Side {
        table: Arc::new(table),
        facts,
    })
}

/// Compare two tables the caller already holds.
///
/// This is the path a settings change takes: the files are not read again, so a
/// key column or a tolerance is applied to exactly the data on screen.
#[must_use]
pub fn spawn_recompare(
    left: Side,
    right: Side,
    settings: TableSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<TableMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            compare_tables(
                left,
                right,
                &settings,
                emitter,
                cancel,
                None,
                SheetContext::default(),
            );
        },
        notify,
    )
}

/// Compare the selected sheet from already loaded sheet inputs.
#[must_use]
pub fn spawn_recompare_sheet(
    sheets: Vec<SheetPair>,
    active_sheet: usize,
    show_sheet_tabs: bool,
    sheet_names: (Vec<String>, Vec<String>),
    settings: TableSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<TableMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let Some(sheet) = sheets.get(active_sheet) else {
                emitter.send(TableMessage::Failed(
                    "The selected sheet is no longer available.".into(),
                ));
                return;
            };
            compare_tables(
                sheet.left.clone(),
                sheet.right.clone(),
                &settings,
                emitter,
                cancel,
                None,
                SheetContext {
                    pairs: sheets,
                    active: active_sheet,
                    show_tabs: show_sheet_tabs,
                    names: sheet_names,
                },
            );
        },
        notify,
    )
}

fn load_and_compare(
    left: &Path,
    right: &Path,
    settings: &TableSettings,
    clipboard: &[Option<String>; 2],
    emitter: &Emitter<TableMessage>,
    cancel: &Cancel,
) {
    emitter.send(TableMessage::Progress(Stage::Reading));
    let left_sheets = match read_sheets(left, &settings.left_format, clipboard[0].as_deref()) {
        Ok(sheets) => sheets,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    let right_sheets = match read_sheets(right, &settings.right_format, clipboard[1].as_deref()) {
        Ok(sheets) => sheets,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(TableMessage::Progress(Stage::Detecting));
    let left_prepared = prepare_sheets(
        left_sheets,
        &settings.left_format,
        settings,
        &settings.schema.left_regional,
    );
    let right_prepared = prepare_sheets(
        right_sheets,
        &settings.right_format,
        settings,
        &settings.schema.right_regional,
    );
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(TableMessage::Progress(Stage::Parsing));
    let left_sheets = match build_sheets(left_prepared, left) {
        Ok(sheets) => sheets,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    let right_sheets = match build_sheets(right_prepared, right) {
        Ok(sheets) => sheets,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    let sheet_names = (
        left_sheets.iter().map(|sheet| sheet.name.clone()).collect(),
        right_sheets
            .iter()
            .map(|sheet| sheet.name.clone())
            .collect(),
    );
    let show_sheet_tabs = (is_workbook_path(left) && clipboard[0].is_none())
        || (is_workbook_path(right) && clipboard[1].is_none());
    let sheets = match pair_sheets(&left_sheets, &right_sheets, &settings.sheets) {
        Ok(sheets) => sheets,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }
    let Some(selected) = sheets.first() else {
        emitter.send(TableMessage::Failed(
            "No sheets were available to compare.".into(),
        ));
        return;
    };
    compare_tables(
        selected.left.clone(),
        selected.right.clone(),
        settings,
        emitter,
        cancel,
        selected.texts.clone(),
        SheetContext {
            pairs: sheets,
            active: 0,
            show_tabs: show_sheet_tabs,
            names: sheet_names,
        },
    );
}

fn compare_tables(
    left: Side,
    right: Side,
    settings: &TableSettings,
    emitter: &Emitter<TableMessage>,
    cancel: &Cancel,
    texts: Option<(Arc<String>, Arc<String>)>,
    sheets: SheetContext,
) {
    emitter.send(TableMessage::Progress(Stage::Schema));
    let schema = Schema::build(&left.table, &right.table, &settings.schema);
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(TableMessage::Progress(Stage::Aligning));
    let alignment =
        match align_cancellable(&left.table, &right.table, &schema, &settings.align, cancel) {
            Ok(alignment) => alignment,
            Err(TableError::Cancelled) => return,
            Err(error) => {
                emitter.send(TableMessage::Failed(error.to_string()));
                return;
            }
        };

    emitter.send(TableMessage::Progress(Stage::Comparing));
    let comparison = match compare_with(
        &left.table,
        &right.table,
        &schema,
        &alignment,
        &settings.compare,
        cancel,
    ) {
        Ok(comparison) => comparison,
        Err(TableError::Cancelled) => return,
        Err(error) => {
            emitter.send(TableMessage::Failed(error.to_string()));
            return;
        }
    };

    emitter.send(TableMessage::Ready(Box::new(TableData {
        left,
        right,
        schema: Arc::new(schema),
        alignment: Arc::new(alignment),
        comparison: Arc::new(comparison),
        texts,
        sheet_pairs: sheets.pairs,
        active_sheet: sheets.active,
        show_sheet_tabs: sheets.show_tabs,
        left_sheet_names: sheets.names.0,
        right_sheet_names: sheets.names.1,
    })));
}

/// The decoded text of one file, with what decoding it cost.
struct SideText {
    text: String,
    encoding: String,
    had_errors: bool,
    stamp: Option<Stamp>,
    template: Arc<LoadedText>,
}

struct LoadedSheet {
    name: String,
    side: Side,
    text: Option<Arc<String>>,
}

struct RawSheet {
    name: String,
    text: String,
    encoding: String,
    had_errors: bool,
    stamp: Option<Stamp>,
    template: Option<Arc<LoadedText>>,
    workbook: bool,
}

struct PreparedSheet {
    raw: RawSheet,
    options: ParseOptions,
}

const MAX_WORKBOOK_SHEETS: usize = 256;
const MAX_WORKBOOK_TEXT_BYTES: usize = ca_table::parse::MAX_INPUT_BYTES;
const MAX_WORKBOOK_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_WORKBOOK_ARCHIVE_ENTRIES: usize = 4_096;

pub(crate) fn is_workbook_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "xls" | "xla" | "xlsx" | "xlsm" | "xlam" | "xlsb" | "ods"
            )
        })
}

#[derive(Clone, Copy)]
enum WorkbookKind {
    Xlsx,
    Xlsb,
}

impl WorkbookKind {
    const fn uses_zip_archive(self) -> bool {
        matches!(self, Self::Xlsx | Self::Xlsb)
    }
}

fn workbook_kind(path: &Path) -> Option<WorkbookKind> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .and_then(|extension| match extension.as_str() {
            "xlsx" | "xlsm" | "xlam" => Some(WorkbookKind::Xlsx),
            "xlsb" => Some(WorkbookKind::Xlsb),
            _ => None,
        })
}

enum StreamingWorkbook<R> {
    Xlsx(calamine::Xlsx<R>),
    Xlsb(calamine::Xlsb<R>),
}

impl<R: std::io::Read + std::io::Seek> StreamingWorkbook<R> {
    fn sheet_names(&self) -> Vec<String> {
        match self {
            Self::Xlsx(reader) => reader.sheet_names().clone(),
            Self::Xlsb(reader) => reader.sheet_names().clone(),
        }
    }

    fn with_cells_reader<'a, F, T>(
        &'a mut self,
        name: &str,
        path: &Path,
        use_reader: F,
    ) -> Result<T, String>
    where
        F: FnOnce(
            &mut dyn FnMut() -> Result<Option<calamine::Cell<calamine::DataRef<'a>>>, String>,
        ) -> Result<T, String>,
    {
        match self {
            Self::Xlsx(workbook) => {
                let mut reader = workbook
                    .worksheet_cells_reader(name)
                    .map_err(|error| format!("{} — {name}: {error}", path.display()))?;
                let mut next_cell = || {
                    reader
                        .next_cell()
                        .map_err(|error| format!("{} — {name}: {error}", path.display()))
                };
                use_reader(&mut next_cell)
            }
            Self::Xlsb(workbook) => {
                let mut reader = workbook
                    .worksheet_cells_reader(name)
                    .map_err(|error| format!("{} — {name}: {error}", path.display()))?;
                let mut next_cell = || {
                    reader
                        .next_cell()
                        .map_err(|error| format!("{} — {name}: {error}", path.display()))
                };
                use_reader(&mut next_cell)
            }
        }
    }
}

fn read_sheets(
    path: &Path,
    format: &FormatSettings,
    clipboard: Option<&str>,
) -> Result<Vec<RawSheet>, String> {
    if clipboard.is_none() && is_workbook_path(path) {
        return read_workbook_sheets(path);
    }
    let input = read_input(path, format, clipboard)?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Ok(vec![RawSheet {
        name,
        text: input.text,
        encoding: input.encoding,
        had_errors: input.had_errors,
        stamp: input.stamp,
        template: Some(input.template),
        workbook: false,
    }])
}

fn prepare_sheets(
    raw_sheets: Vec<RawSheet>,
    format: &FormatSettings,
    settings: &TableSettings,
    regional: &ca_table::regional::Regional,
) -> Vec<PreparedSheet> {
    raw_sheets
        .into_iter()
        .map(|raw| {
            let options = if raw.workbook {
                ParseOptions {
                    syntax: ca_table::parse::FieldSyntax::delimited(['\t']),
                    ..ParseOptions::default()
                }
            } else {
                resolve(&raw.text, format, settings, regional)
            };
            PreparedSheet { raw, options }
        })
        .collect()
}

fn build_sheets(prepared: Vec<PreparedSheet>, path: &Path) -> Result<Vec<LoadedSheet>, String> {
    prepared
        .into_iter()
        .map(|prepared| {
            let PreparedSheet { raw, options } = prepared;
            let table = parse(&raw.text, &options)
                .map_err(|error| format!("{} — {}: {error}", path.display(), raw.name))?;
            let mut resolved_options = options;
            resolved_options.first_line_contains = table.first_line_contains().clone();
            let facts = SideFacts {
                encoding: raw.encoding,
                had_errors: raw.had_errors,
                syntax: if raw.workbook {
                    "Spreadsheet cells".to_owned()
                } else {
                    syntax_label(&resolved_options)
                },
                header: header_label(&table),
                warnings: table.warnings().to_vec(),
                options: resolved_options,
                stamp: raw.stamp,
                template: raw.template,
            };
            Ok(LoadedSheet {
                name: raw.name,
                side: Side {
                    table: Arc::new(table),
                    facts,
                },
                text: (!raw.workbook).then(|| Arc::new(raw.text)),
            })
        })
        .collect()
}

fn read_workbook_sheets(path: &Path) -> Result<Vec<RawSheet>, String> {
    let kind = workbook_kind(path).ok_or_else(|| {
        format!(
            "{} uses a workbook format that Table Compare cannot load safely; use .xlsx, .xlsm, .xlam, or .xlsb",
            path.display()
        )
    })?;
    let size = std::fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    let limit = u64::try_from(ca_table::parse::MAX_INPUT_BYTES).unwrap_or(u64::MAX);
    if size > limit {
        return Err(format!(
            "{} is {size} bytes, past the {limit} byte workbook input limit",
            path.display()
        ));
    }
    let stamp = RealFileSystem.stamp(path);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() > ca_table::parse::MAX_INPUT_BYTES {
        return Err(format!(
            "{} grew past the {limit} byte workbook input limit while it was read",
            path.display()
        ));
    }
    if kind.uses_zip_archive() {
        check_workbook_archive(path, &bytes)?;
    }
    let mut workbook = match kind {
        WorkbookKind::Xlsb => StreamingWorkbook::Xlsb(
            calamine::open_workbook_from_rs::<calamine::Xlsb<_>, _>(std::io::Cursor::new(bytes))
                .map_err(|error| format!("{}: {error}", path.display()))?,
        ),
        WorkbookKind::Xlsx => StreamingWorkbook::Xlsx(
            calamine::open_workbook_from_rs::<calamine::Xlsx<_>, _>(std::io::Cursor::new(bytes))
                .map_err(|error| format!("{}: {error}", path.display()))?,
        ),
    };
    let names = workbook.sheet_names();
    if names.len() > MAX_WORKBOOK_SHEETS {
        return Err(format!(
            "{} has {} sheets, past the {MAX_WORKBOOK_SHEETS} sheet limit",
            path.display(),
            names.len()
        ));
    }
    let mut sheets = Vec::with_capacity(names.len());
    let mut total_bytes = 0usize;
    for name in names {
        let text = read_streaming_sheet(&mut workbook, &name, path, &mut total_bytes)?;
        sheets.push(RawSheet {
            name,
            text,
            encoding: "Spreadsheet values".to_owned(),
            had_errors: false,
            stamp,
            template: None,
            workbook: true,
        });
    }
    Ok(sheets)
}

fn check_workbook_archive(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if archive.len() > MAX_WORKBOOK_ARCHIVE_ENTRIES {
        return Err(format!(
            "{} has {} archive entries, past the {MAX_WORKBOOK_ARCHIVE_ENTRIES} entry workbook limit",
            path.display(),
            archive.len()
        ));
    }
    let mut expanded_bytes = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        expanded_bytes = expanded_bytes.saturating_add(entry.size());
        if expanded_bytes > MAX_WORKBOOK_ARCHIVE_BYTES {
            return Err(format!(
                "{} expands past the {} byte workbook archive limit",
                path.display(),
                MAX_WORKBOOK_ARCHIVE_BYTES
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct SheetBounds {
    first_row: u32,
    last_row: u32,
    first_column: u32,
    last_column: u32,
}

fn read_streaming_sheet<R: std::io::Read + std::io::Seek>(
    workbook: &mut StreamingWorkbook<R>,
    name: &str,
    path: &Path,
    total_bytes: &mut usize,
) -> Result<String, String> {
    let bounds = workbook.with_cells_reader(name, path, |next_cell| {
        scan_sheet_bounds(next_cell, path, name)
    })?;
    let Some(bounds) = bounds else {
        return Ok(String::new());
    };
    workbook.with_cells_reader(name, path, |next_cell| {
        stream_sheet_text(
            || next_cell().map(|cell| cell.map(|cell| (cell.get_position(), cell))),
            bounds,
            path,
            name,
            total_bytes,
        )
    })
}

fn scan_sheet_bounds<'a>(
    mut next_cell: impl FnMut() -> Result<Option<calamine::Cell<calamine::DataRef<'a>>>, String>,
    path: &Path,
    sheet: &str,
) -> Result<Option<SheetBounds>, String> {
    let mut bounds: Option<SheetBounds> = None;
    while let Some(cell) = next_cell()? {
        if cell.is_empty() {
            continue;
        }
        let (row, column) = cell.get_position();
        let current = bounds.get_or_insert(SheetBounds {
            first_row: row,
            last_row: row,
            first_column: column,
            last_column: column,
        });
        current.first_row = current.first_row.min(row);
        current.last_row = current.last_row.max(row);
        current.first_column = current.first_column.min(column);
        current.last_column = current.last_column.max(column);

        let rows = u64::from(current.last_row) - u64::from(current.first_row) + 1;
        let columns = u64::from(current.last_column) - u64::from(current.first_column) + 1;
        let cells = rows.saturating_mul(columns);
        if rows > ca_table::parse::MAX_TABLE_ROWS as u64
            || cells > ca_table::parse::MAX_TABLE_CELLS as u64
        {
            return Err(format!(
                "{} — {sheet} is past the {} row or {} cell table limit",
                path.display(),
                ca_table::parse::MAX_TABLE_ROWS,
                ca_table::parse::MAX_TABLE_CELLS
            ));
        }
    }
    Ok(bounds)
}

fn stream_sheet_text<'a>(
    mut next_cell: impl FnMut() -> Result<
        Option<((u32, u32), calamine::Cell<calamine::DataRef<'a>>)>,
        String,
    >,
    bounds: SheetBounds,
    path: &Path,
    sheet: &str,
    total_bytes: &mut usize,
) -> Result<String, String> {
    let mut output = String::new();
    let mut current_row = bounds.first_row;
    let mut last_column = None;
    let mut previous_position = None;
    while let Some(((row, column), cell)) = next_cell()? {
        if cell.is_empty() {
            continue;
        }
        if let Some(previous) = previous_position {
            if (row, column) <= previous {
                return Err(format!(
                    "{} — {sheet} has cells outside row order",
                    path.display()
                ));
            }
        }
        while current_row < row {
            output.push('\n');
            current_row += 1;
            last_column = None;
        }
        let tabs = last_column.map_or_else(
            || column - bounds.first_column,
            |previous| column - previous,
        );
        for _ in 0..tabs {
            output.push('\t');
        }
        let value = cell.text();
        append_workbook_cell(&mut output, value.as_ref(), path, sheet, *total_bytes)?;
        last_column = Some(column);
        current_row = row;
        previous_position = Some((row, column));
    }
    if output.len() > MAX_WORKBOOK_TEXT_BYTES
        || total_bytes.saturating_add(output.len()) > MAX_WORKBOOK_TEXT_BYTES
    {
        return Err(workbook_text_limit_error(path, sheet));
    }
    *total_bytes = total_bytes.saturating_add(output.len());
    Ok(output)
}

fn append_workbook_cell(
    output: &mut String,
    value: &str,
    path: &Path,
    sheet: &str,
    previous_bytes: usize,
) -> Result<(), String> {
    if value.len() > MAX_WORKBOOK_TEXT_BYTES
        || previous_bytes
            .saturating_add(output.len())
            .saturating_add(value.len())
            > MAX_WORKBOOK_TEXT_BYTES
    {
        return Err(workbook_text_limit_error(path, sheet));
    }
    let quoted = value
        .chars()
        .any(|character| matches!(character, '\t' | '\n' | '\r' | '"'))
        || value.trim() != value;
    let encoded_len = value
        .len()
        .saturating_add(if quoted { 2 } else { 0 })
        .saturating_add(if quoted {
            value.chars().filter(|character| *character == '"').count()
        } else {
            0
        });
    if previous_bytes
        .saturating_add(output.len())
        .saturating_add(encoded_len)
        > MAX_WORKBOOK_TEXT_BYTES
    {
        return Err(workbook_text_limit_error(path, sheet));
    }
    if quoted {
        output.push('"');
        for character in value.chars() {
            output.push(character);
            if character == '"' {
                output.push('"');
            }
        }
        output.push('"');
    } else {
        output.push_str(value);
    }
    Ok(())
}

fn workbook_text_limit_error(path: &Path, sheet: &str) -> String {
    format!(
        "{} — {sheet} expands past the {} byte workbook table limit",
        path.display(),
        MAX_WORKBOOK_TEXT_BYTES
    )
}

trait WorkbookCellValue {
    fn is_empty(&self) -> bool;
    fn text(&self) -> std::borrow::Cow<'_, str>;
}

impl WorkbookCellValue for calamine::Cell<calamine::DataRef<'_>> {
    fn is_empty(&self) -> bool {
        matches!(self.get_value(), calamine::DataRef::Empty)
    }

    fn text(&self) -> std::borrow::Cow<'_, str> {
        match self.get_value() {
            calamine::DataRef::DateTime(value) if value.is_datetime() => {
                let (year, month, day, hour, minute, second, millisecond) =
                    value.to_ymd_hms_milli();
                std::borrow::Cow::Owned(format!(
                "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}"
            ))
            }
            calamine::DataRef::String(value)
            | calamine::DataRef::DateTimeIso(value)
            | calamine::DataRef::DurationIso(value) => std::borrow::Cow::Borrowed(value.as_str()),
            calamine::DataRef::SharedString(value) => std::borrow::Cow::Borrowed(value),
            value => std::borrow::Cow::Owned(calamine::Data::from(value.clone()).to_string()),
        }
    }
}

fn pair_sheets(
    left: &[LoadedSheet],
    right: &[LoadedSheet],
    pairing: &ca_session::settings::table::TablePairing,
) -> Result<Vec<SheetPair>, String> {
    let indexes = pair_sheet_indexes(left, right, pairing);

    let empty = empty_side()?;
    let mut result = Vec::with_capacity(indexes.len().max(1));
    for (left_index, right_index) in indexes {
        let left_sheet = left_index.and_then(|index| left.get(index));
        let right_sheet = right_index.and_then(|index| right.get(index));
        if left_sheet.is_none() && right_sheet.is_none() {
            continue;
        }
        let left_name = left_sheet.map(|sheet| sheet.name.clone());
        let right_name = right_sheet.map(|sheet| sheet.name.clone());
        let label = pair_label(left_name.as_deref(), right_name.as_deref());
        let left_index = left_sheet
            .and(left_index)
            .and_then(|index| u32::try_from(index).ok());
        let right_index = right_sheet
            .and(right_index)
            .and_then(|index| u32::try_from(index).ok());
        let texts = match (
            left_sheet.and_then(|sheet| sheet.text.clone()),
            right_sheet.and_then(|sheet| sheet.text.clone()),
        ) {
            (Some(left), Some(right)) => Some((left, right)),
            _ => None,
        };
        result.push(SheetPair {
            label,
            left_name,
            right_name,
            left_index,
            right_index,
            left: left_sheet.map_or_else(|| empty.clone(), |sheet| sheet.side.clone()),
            right: right_sheet.map_or_else(|| empty.clone(), |sheet| sheet.side.clone()),
            texts,
        });
    }
    if result.is_empty() {
        result.push(SheetPair {
            label: "No sheets paired".to_owned(),
            left_name: None,
            right_name: None,
            left_index: None,
            right_index: None,
            left: empty.clone(),
            right: empty,
            texts: None,
        });
    }
    Ok(result)
}

fn pair_sheet_indexes(
    left: &[LoadedSheet],
    right: &[LoadedSheet],
    pairing: &ca_session::settings::table::TablePairing,
) -> Vec<(Option<usize>, Option<usize>)> {
    use ca_session::settings::table::TablePairing;

    match pairing {
        TablePairing::ByLeftName { .. } => {
            let mut used = vec![false; right.len()];
            let mut pairs = Vec::with_capacity(left.len() + right.len());
            for (left_index, sheet) in left.iter().enumerate() {
                let right_index = right.iter().enumerate().find_map(|(index, candidate)| {
                    (!used[index]
                        && normalized_sheet_name(&sheet.name)
                            == normalized_sheet_name(&candidate.name))
                    .then_some(index)
                });
                if let Some(index) = right_index {
                    used[index] = true;
                }
                pairs.push((Some(left_index), right_index));
            }
            pairs.extend(
                used.iter()
                    .enumerate()
                    .filter_map(|(index, used)| (!used).then_some((None, Some(index)))),
            );
            pairs
        }
        TablePairing::ByRightName { .. } => {
            let mut used = vec![false; left.len()];
            let mut pairs = Vec::with_capacity(left.len() + right.len());
            for (right_index, sheet) in right.iter().enumerate() {
                let left_index = left.iter().enumerate().find_map(|(index, candidate)| {
                    (!used[index]
                        && normalized_sheet_name(&sheet.name)
                            == normalized_sheet_name(&candidate.name))
                    .then_some(index)
                });
                if let Some(index) = left_index {
                    used[index] = true;
                }
                pairs.push((left_index, Some(right_index)));
            }
            pairs.extend(
                used.iter()
                    .enumerate()
                    .filter_map(|(index, used)| (!used).then_some((Some(index), None))),
            );
            pairs
        }
        TablePairing::Custom { pairs, .. } => pairs
            .iter()
            .filter_map(|(left, right)| {
                let left = left.and_then(|index| usize::try_from(index).ok());
                let right = right.and_then(|index| usize::try_from(index).ok());
                (left.is_some() || right.is_some()).then_some((left, right))
            })
            .collect(),
        _ => (0..left.len().max(right.len()))
            .map(|index| {
                (
                    (index < left.len()).then_some(index),
                    (index < right.len()).then_some(index),
                )
            })
            .collect(),
    }
}

fn normalized_sheet_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn pair_label(left: Option<&str>, right: Option<&str>) -> String {
    match (left, right) {
        (Some(left), Some(right))
            if normalized_sheet_name(left) == normalized_sheet_name(right) =>
        {
            left.to_owned()
        }
        (Some(left), Some(right)) => format!("{left} ↔ {right}"),
        (Some(left), None) => format!("{left} (left only)"),
        (None, Some(right)) => format!("{right} (right only)"),
        (None, None) => "No sheets paired".to_owned(),
    }
}

fn empty_side() -> Result<Side, String> {
    let options = ParseOptions::default();
    let table = parse("", &options).map_err(|error| error.to_string())?;
    Ok(Side {
        table: Arc::new(table),
        facts: SideFacts::default(),
    })
}

fn read_side(path: &Path, format: &FormatSettings) -> Result<SideText, String> {
    let file_size = std::fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    read_side_with_open(path, format, file_size, || std::fs::File::open(path))
}

fn read_side_with_open<R: Read>(
    path: &Path,
    format: &FormatSettings,
    file_size: u64,
    open: impl FnOnce() -> std::io::Result<R>,
) -> Result<SideText, String> {
    let input_limit = u64::try_from(ca_table::parse::MAX_INPUT_BYTES).unwrap_or(u64::MAX);
    if file_size > input_limit {
        return Err(format!(
            "{} is {file_size} bytes, past the {input_limit} byte table input limit",
            path.display()
        ));
    }
    // The stamp is taken before the read, so a change made while the bytes are
    // read is reported by the next save rather than overwritten.
    let stamp = RealFileSystem.stamp(path);
    let file = open().map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(input_limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() > ca_table::parse::MAX_INPUT_BYTES {
        return Err(format!(
            "{} grew past the {input_limit} byte table input limit while it was read",
            path.display()
        ));
    }
    let options = DecodeOptions {
        forced: format.encoding,
        ..DecodeOptions::default()
    };
    let mut loaded = LoadedText::load(&bytes, &options);
    let text = loaded.buffer.text();
    loaded.buffer = TextBuffer::from_text("");
    Ok(SideText {
        encoding: loaded.spec.encoding.label().to_string(),
        had_errors: loaded.had_errors,
        text,
        stamp,
        template: Arc::new(loaded),
    })
}

fn read_input(
    path: &Path,
    format: &FormatSettings,
    clipboard: Option<&str>,
) -> Result<SideText, String> {
    if let Some(text) = clipboard {
        if text.len() > ca_table::parse::MAX_INPUT_BYTES {
            return Err(format!(
                "clipboard text is past the {} byte table input limit",
                ca_table::parse::MAX_INPUT_BYTES
            ));
        }
        let options = DecodeOptions {
            forced: Some(ca_text::TextEncoding::Utf8),
            ..DecodeOptions::default()
        };
        let mut loaded = LoadedText::load(text.as_bytes(), &options);
        let decoded = loaded.buffer.text();
        loaded.buffer = TextBuffer::from_text("");
        return Ok(SideText {
            text: decoded,
            encoding: loaded.spec.encoding.label().to_owned(),
            had_errors: loaded.had_errors,
            stamp: None,
            template: Arc::new(loaded),
        });
    }
    read_side(path, format)
}

/// The parse options in force for one side.
///
/// Detection reads a sample of the data, so a side whose options are pinned
/// never pays for it.
fn resolve(
    text: &str,
    format: &FormatSettings,
    settings: &TableSettings,
    regional: &ca_table::regional::Regional,
) -> ParseOptions {
    if let Some(options) = &format.parse {
        return options.clone();
    }
    let detected = detect(text, &settings.detect, regional);
    ParseOptions {
        syntax: detected.syntax,
        first_line_contains: detected.first_line_contains,
        ..ParseOptions::default()
    }
}

/// A one word description of a field syntax, for the information line.
fn syntax_label(options: &ParseOptions) -> String {
    use ca_table::parse::FieldSyntax;
    match &options.syntax {
        FieldSyntax::Delimited { delimiters, .. } => {
            let names: Vec<String> = delimiters.iter().map(|ch| delimiter_name(*ch)).collect();
            format!("Delimited by {}", names.join(" "))
        }
        FieldSyntax::Fixed { column_widths, .. } => {
            format!("Fixed width, {} columns", column_widths.len())
        }
        FieldSyntax::Detect { .. } => "Detected".to_string(),
        _ => "Delimited".to_string(),
    }
}

fn delimiter_name(ch: char) -> String {
    match ch {
        '\t' => "tab".to_string(),
        ' ' => "space".to_string(),
        ',' => "comma".to_string(),
        ';' => "semicolon".to_string(),
        '|' => "bar".to_string(),
        other => other.to_string(),
    }
}

fn header_label(table: &Table) -> String {
    use ca_table::parse::FirstLineContains;
    match table.first_line_contains() {
        FirstLineContains::ColumnNames => "Line one names the columns".to_string(),
        FirstLineContains::CellData => "Line one holds data".to_string(),
        _ => "Line one not decided".to_string(),
    }
}

/// The running comparison, and the promise that only its result is used.
///
/// Every request gets a generation number. Starting a new one drops the old
/// job, which raises its flag, and raises the superseded count. A result that
/// was already in flight cannot reach the view, because the handle it would
/// arrive through no longer exists.
#[derive(Default)]
pub struct Pipeline {
    job: Option<Job<TableMessage>>,
    generation: u64,
    superseded: u64,
}

impl Pipeline {
    /// A pipeline with nothing running.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take over from whatever was running, and return the new generation.
    pub fn start(&mut self, job: Job<TableMessage>) -> u64 {
        if let Some(previous) = self.job.take() {
            previous.cancel();
            self.superseded += 1;
        }
        self.generation += 1;
        self.job = Some(job);
        self.generation
    }

    /// Take whatever the running job has posted.
    pub fn poll(&mut self) -> Vec<TableMessage> {
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

    /// True while a job is running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// The generation of the request currently running.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// How many requests were replaced before they finished.
    #[must_use]
    pub const fn superseded_count(&self) -> u64 {
        self.superseded
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        read_side_with_open, read_workbook_sheets, spawn_load, spawn_recompare, FormatSettings,
        Pipeline, Side, Stage, TableMessage, TableSettings,
    };
    use ca_table::compare::CellStatus;
    use ca_table::schema::{ColumnAlignment, ColumnHandling, ColumnType, ManualColumnPair};
    use std::fmt::Write as _;
    use std::io::{Read as IoRead, Write as IoWrite};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn drain(job: &mut ca_ui::worker::Job<TableMessage>) -> Vec<TableMessage> {
        let deadline = Instant::now() + Duration::from_secs(30);
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

    fn ready(messages: Vec<TableMessage>) -> Box<super::TableData> {
        messages
            .into_iter()
            .find_map(|message| match message {
                TableMessage::Ready(data) => Some(data),
                _ => None,
            })
            .expect("the comparison finished")
    }

    fn run(left: &Path, right: &Path, settings: TableSettings) -> Box<super::TableData> {
        let mut job = spawn_load(
            left.to_path_buf(),
            right.to_path_buf(),
            settings,
            Arc::new(|| {}),
        );
        ready(drain(&mut job))
    }

    fn fixture(
        left_text: &str,
        right_text: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.csv");
        let right = dir.path().join("right.csv");
        std::fs::write(&left, left_text).unwrap();
        std::fs::write(&right, right_text).unwrap();
        (dir, left, right)
    }

    #[test]
    fn a_table_file_over_the_input_limit_is_refused_before_reading_it() {
        let input_limit = u64::try_from(ca_table::parse::MAX_INPUT_BYTES).unwrap();
        let path = Path::new("oversized.csv");
        let mut opened = false;
        let result = read_side_with_open(path, &FormatSettings::default(), input_limit + 1, || {
            opened = true;
            Ok(std::io::Cursor::new(b"a,b\n1,2\n".to_vec()))
        });

        assert!(result.is_err());
        assert!(
            !opened,
            "the file reader must not be opened for an oversized input"
        );
    }

    #[test]
    fn two_files_reach_a_comparison() {
        let (_dir, left, right) = fixture(
            "id,name,score\n1,Ann,10\n2,Bob,20\n",
            "id,name,score\n1,Ann,11\n2,Bob,20\n",
        );
        let data = run(&left, &right, TableSettings::default());
        assert_eq!(data.left.table.row_count(), 2);
        assert_eq!(data.schema.columns.len(), 3);
        assert_eq!(data.alignment.len(), 2);
        assert_eq!(data.comparison.totals.rows_with_differences(), 1);
        assert_eq!(data.left.facts.encoding, "UTF-8");
    }

    fn workbook_bytes_with_sheet1_rewrite(
        rewrite_sheet1: impl FnOnce(String) -> String,
    ) -> Vec<u8> {
        let fixture = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/multiple-left.xlsx"
        ));
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(fixture.as_slice())).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let mut rewrite_sheet1 = Some(rewrite_sheet1);
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().unwrap().into_owned();
            let is_directory = entry.is_dir();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name == "xl/worksheets/sheet1.xml" {
                let xml = String::from_utf8(bytes).unwrap();
                let xml = rewrite_sheet1.take().unwrap()(xml);
                bytes = xml.into_bytes();
            }
            let options = zip::write::SimpleFileOptions::default();
            if is_directory {
                writer.add_directory(name, options).unwrap();
            } else {
                writer.start_file(name, options).unwrap();
                writer.write_all(&bytes).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn sparse_workbook_bytes() -> Vec<u8> {
        workbook_bytes_with_sheet1_rewrite(|xml| {
            xml.replace("A1:B2", "A1:XFD1048576").replace(
                "</sheetData>",
                "<row r=\"3\"><c r=\"A3\"/></row><row r=\"1048576\"><c r=\"XFD1048576\"><v>2</v></c></row></sheetData>",
            )
        })
    }

    fn workbook_with_leading_empty_cell_bytes() -> Vec<u8> {
        workbook_bytes_with_sheet1_rewrite(|mut xml| {
            xml = xml.replace("A1:B2", "A1:A2");
            let sheet_data_start = xml.find("<sheetData>").unwrap();
            let sheet_data_end = xml.find("</sheetData>").unwrap() + "</sheetData>".len();
            xml.replace_range(
                sheet_data_start..sheet_data_end,
                "<sheetData><row r=\"1\"><c r=\"A1\"/></row><row r=\"2\"><c r=\"A2\" t=\"inlineStr\"><is><t>later</t></is></c></row></sheetData>",
            );
            xml
        })
    }

    #[test]
    fn sparse_workbook_is_rejected_before_dense_range_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.xlsx");
        std::fs::write(&path, sparse_workbook_bytes()).unwrap();

        assert!(read_workbook_sheets(&path).is_err_and(|error| error.contains("table limit")));
    }

    #[test]
    fn leading_empty_cell_does_not_end_the_workbook_bounds_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leading-empty.xlsx");
        std::fs::write(&path, workbook_with_leading_empty_cell_bytes()).unwrap();

        let sheets = read_workbook_sheets(&path).unwrap();

        assert!(sheets[0].text.contains("later"));
    }

    #[test]
    fn workbook_sheets_can_pair_by_normalized_name_and_keep_unpaired_sheets() {
        let left = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multiple-left.xlsx");
        let right =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multiple-right.xlsx");
        let mut settings = TableSettings::default();
        settings.sheets = ca_session::settings::table::TablePairing::by_left_name();
        let data = run(&left, &right, settings);

        assert!(data.show_sheet_tabs);
        assert_eq!(data.sheet_pairs.len(), 4);
        assert_eq!(data.sheet_pairs[0].label, "One");
        assert_eq!(data.sheet_pairs[0].left_name.as_deref(), Some("One"));
        assert_eq!(data.sheet_pairs[0].right_name.as_deref(), Some("one"));
        assert_eq!(data.comparison.totals.rows_with_differences(), 0);
        assert_eq!(data.sheet_pairs[1].label, "Shared A");
        assert_eq!(data.sheet_pairs[2].label, "Only Left (left only)");
        assert_eq!(data.sheet_pairs[3].label, "Only Right (right only)");
        assert!(data.sheet_pairs[2].right_name.is_none());
        assert!(data.sheet_pairs[3].left_name.is_none());
    }

    #[test]
    fn right_name_pairing_keeps_right_order_and_appends_left_only_sheets() {
        let left = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multiple-left.xlsx");
        let right =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multiple-right.xlsx");
        let mut settings = TableSettings::default();
        settings.sheets = ca_session::settings::table::TablePairing::by_right_name();
        let data = run(&left, &right, settings);

        assert_eq!(data.sheet_pairs.len(), 4);
        assert_eq!(data.sheet_pairs[0].label, "Shared A");
        assert_eq!(data.sheet_pairs[0].left_index, Some(1));
        assert_eq!(data.sheet_pairs[0].right_index, Some(0));
        assert_eq!(data.sheet_pairs[1].label, "One");
        assert_eq!(data.sheet_pairs[2].label, "Only Right (right only)");
        assert_eq!(data.sheet_pairs[3].label, "Only Left (left only)");
    }

    #[test]
    fn workbook_sheets_pair_by_position_or_follow_custom_order() {
        let left = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multiple-left.xlsx");
        let right =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multiple-right.xlsx");
        let positional = run(&left, &right, TableSettings::default());
        assert_eq!(positional.sheet_pairs.len(), 3);
        assert_eq!(positional.sheet_pairs[0].label, "One ↔ SharedA");
        assert!(positional.comparison.totals.rows_with_differences() > 0);

        let mut settings = TableSettings::default();
        settings.sheets = ca_session::settings::table::TablePairing::Custom {
            pairs: vec![(Some(1), Some(0)), (Some(2), None), (None, Some(2))],
            unknown: std::collections::BTreeMap::new(),
        };
        let custom = run(&left, &right, settings);
        assert_eq!(custom.sheet_pairs.len(), 3);
        assert_eq!(custom.sheet_pairs[0].label, "Shared A");
        assert_eq!(custom.sheet_pairs[1].label, "Only Left (left only)");
        assert_eq!(custom.sheet_pairs[2].label, "Only Right (right only)");

        let mut job = super::spawn_recompare_sheet(
            custom.sheet_pairs.clone(),
            1,
            true,
            (
                vec!["One".into(), "Shared A".into(), "Only Left".into()],
                vec!["SharedA".into(), "one".into(), "Only Right".into()],
            ),
            TableSettings::default(),
            Arc::new(|| {}),
        );
        let selected = ready(drain(&mut job));
        assert_eq!(selected.active_sheet, 1);
        assert!(selected.show_sheet_tabs);
        assert_eq!(selected.left.table.row_count(), 1);
        assert_eq!(selected.right.table.row_count(), 0);
    }

    #[test]
    fn every_stage_reports_before_the_result() {
        let (_dir, left, right) = fixture("a,b\n1,2\n", "a,b\n1,3\n");
        let mut job = spawn_load(left, right, TableSettings::default(), Arc::new(|| {}));
        let messages = drain(&mut job);
        let stages: Vec<super::Stage> = messages
            .iter()
            .filter_map(|message| match message {
                TableMessage::Progress(stage) => Some(*stage),
                _ => None,
            })
            .collect();
        assert_eq!(
            stages,
            vec![
                super::Stage::Reading,
                super::Stage::Detecting,
                super::Stage::Parsing,
                super::Stage::Schema,
                super::Stage::Aligning,
                super::Stage::Comparing,
            ]
        );
    }

    #[test]
    fn a_missing_file_reports_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("present.csv");
        std::fs::write(&left, "a\n1\n").unwrap();
        let mut job = spawn_load(
            left,
            dir.path().join("absent.csv"),
            TableSettings::default(),
            Arc::new(|| {}),
        );
        let messages = drain(&mut job);
        assert!(messages
            .iter()
            .any(|message| matches!(message, TableMessage::Failed(_))));
    }

    #[test]
    fn a_key_column_changes_the_alignment_without_rereading() {
        let (_dir, left, right) = fixture("id,name\n2,Bob\n1,Ann\n", "id,name\n1,Ann\n2,Bob\n");
        let data = run(&left, &right, TableSettings::default());
        let mut settings = TableSettings::default();
        // The rows sit in opposite orders, so the key pairs cross unless both
        // sides are sorted first.
        settings.align.sort_rows_before_alignment = true;
        settings.schema.handling.insert(
            0,
            ColumnHandling {
                key: true,
                use_default: false,
                ..ColumnHandling::default()
            },
        );
        let mut job = spawn_recompare(
            Side {
                table: Arc::clone(&data.left.table),
                facts: data.left.facts.clone(),
            },
            Side {
                table: Arc::clone(&data.right.table),
                facts: data.right.facts.clone(),
            },
            settings,
            Arc::new(|| {}),
        );
        let keyed = ready(drain(&mut job));
        assert!(keyed.alignment.keyed);
        assert_eq!(keyed.comparison.totals.rows_with_differences(), 0);
    }

    #[test]
    fn a_custom_column_pair_reads_the_hand_chosen_columns_instead_of_position() {
        let (_dir, left, right) = fixture("a,b\n1,2\n", "x,a\n9,1\n");
        let data = run(&left, &right, TableSettings::default());
        // Positionally, comparison column zero is left "a" against right "x":
        // 1 against 9, a difference.
        assert_eq!(data.comparison.cell(0, 0), Some(CellStatus::Different));

        let mut settings = TableSettings::default();
        settings.schema.alignment = ColumnAlignment::Custom;
        settings.schema.custom = vec![
            ManualColumnPair {
                left: Some(0),
                right: Some(1),
                ..ManualColumnPair::default()
            },
            ManualColumnPair {
                left: Some(1),
                right: Some(0),
                ..ManualColumnPair::default()
            },
        ];
        let mut job = spawn_recompare(
            Side {
                table: Arc::clone(&data.left.table),
                facts: data.left.facts.clone(),
            },
            Side {
                table: Arc::clone(&data.right.table),
                facts: data.right.facts.clone(),
            },
            settings,
            Arc::new(|| {}),
        );
        let paired = ready(drain(&mut job));
        // The hand written pair reads left "a" against right "a": 1 against 1.
        assert_eq!(paired.comparison.cell(0, 0), Some(CellStatus::Same));
    }

    #[test]
    fn the_column_type_decides_whether_differently_written_numbers_compare_equal() {
        let (_dir, left, right) = fixture("n\n1.0\n", "n\n1\n");
        let mut text_settings = TableSettings::default();
        text_settings.schema.handling.insert(
            0,
            ColumnHandling {
                column_type: ColumnType::Text,
                use_default: false,
                ..ColumnHandling::default()
            },
        );
        let as_text = run(&left, &right, text_settings);
        assert_eq!(as_text.comparison.cell(0, 0), Some(CellStatus::Different));

        let mut number_settings = TableSettings::default();
        number_settings.schema.handling.insert(
            0,
            ColumnHandling {
                column_type: ColumnType::Number,
                use_default: false,
                ..ColumnHandling::default()
            },
        );
        let as_number = run(&left, &right, number_settings);
        assert_eq!(as_number.comparison.cell(0, 0), Some(CellStatus::Same));
    }

    #[test]
    fn a_numeric_tolerance_absorbs_a_small_difference_but_not_a_large_one() {
        let (_dir, left, right) = fixture("n\n1.0\n", "n\n1.2\n");
        let mut settings = TableSettings::default();
        settings.schema.handling.insert(
            0,
            ColumnHandling {
                column_type: ColumnType::Number,
                use_default: false,
                numeric_tolerance: 0.5,
                ..ColumnHandling::default()
            },
        );
        let tolerant = run(&left, &right, settings.clone());
        assert_eq!(
            tolerant.comparison.cell(0, 0),
            Some(CellStatus::Unimportant)
        );

        settings.schema.handling.insert(
            0,
            ColumnHandling {
                column_type: ColumnType::Number,
                use_default: false,
                numeric_tolerance: 0.05,
                ..ColumnHandling::default()
            },
        );
        let strict = run(&left, &right, settings);
        assert_eq!(strict.comparison.cell(0, 0), Some(CellStatus::Different));
    }

    #[test]
    fn never_aligning_differences_splits_a_changed_row_into_added_and_deleted() {
        let (_dir, left, right) = fixture("a,b\n1,x\n2,y\n", "a,b\n1,x\n9,q\n");
        let mut settings = TableSettings::default();
        let data = run(&left, &right, settings.clone());
        // By default a changed row still pairs, so row one differs in place.
        assert!(data
            .alignment
            .pairs
            .contains(&ca_table::align::RowPair::both(1, 1)));

        settings.align.never_align_differences = true;
        settings.align.use_closeness_matching = false;
        let split = run(&left, &right, settings);
        // With the option on, the changed row is a deletion and an insertion
        // instead of a paired change.
        assert!(!split
            .alignment
            .pairs
            .contains(&ca_table::align::RowPair::both(1, 1)));
        assert!(split
            .alignment
            .pairs
            .contains(&ca_table::align::RowPair::left_only(1)));
        assert!(split
            .alignment
            .pairs
            .contains(&ca_table::align::RowPair::right_only(1)));
    }

    #[test]
    fn closeness_matching_finds_the_pair_a_plain_sequence_diff_would_miss() {
        let (_dir, left, right) =
            fixture("line\nalpha1\nbeta2\n", "line\nnewrow\nalpha11\nbeta22\n");
        let mut settings = TableSettings::default();
        settings.align.use_closeness_matching = true;
        let matched = run(&left, &right, settings.clone());
        // The insertion sits before both changed rows. Closeness matching
        // still finds that row two is closer to "beta22" than to "newrow".
        assert!(matched
            .alignment
            .pairs
            .contains(&ca_table::align::RowPair::both(1, 2)));

        settings.align.use_closeness_matching = false;
        let unmatched = run(&left, &right, settings);
        // Without it, the sequence engine pairs the changed rows by their
        // position in the hunk instead, so that same pairing disappears.
        assert!(!unmatched
            .alignment
            .pairs
            .contains(&ca_table::align::RowPair::both(1, 2)));
    }

    #[test]
    fn a_cancelled_run_reaches_a_terminal_message() {
        let mut wide = String::new();
        for row in 0..2_000 {
            let _ = writeln!(wide, "{row},{},{}", row * 3, row * 7);
        }
        let (_dir, left, right) = fixture(&format!("a,b,c\n{wide}"), &format!("a,b,c\n{wide}"));
        let mut job = spawn_load(left, right, TableSettings::default(), Arc::new(|| {}));
        job.cancel();
        let messages = drain(&mut job);
        assert!(job.is_finished());
        assert_eq!(
            messages
                .iter()
                .filter(|message| matches!(
                    message,
                    TableMessage::Ready(_) | TableMessage::Cancelled | TableMessage::Failed(_)
                ))
                .count(),
            1,
            "a run must end with exactly one terminal message"
        );
    }

    #[test]
    fn a_newer_request_supersedes_an_older_one() {
        let (_dir, left, right) = fixture("a,b\n1,2\n", "a,b\n1,3\n");
        let mut pipeline = Pipeline::new();
        let first = pipeline.start(spawn_load(
            left.clone(),
            right.clone(),
            TableSettings::default(),
            Arc::new(|| {}),
        ));
        let second = pipeline.start(spawn_load(
            left,
            right,
            TableSettings::default(),
            Arc::new(|| {}),
        ));
        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(pipeline.superseded_count(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut ready_count = 0;
        while Instant::now() < deadline {
            for message in pipeline.poll() {
                if matches!(message, TableMessage::Ready(_)) {
                    ready_count += 1;
                }
            }
            if !pipeline.is_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(ready_count, 1, "only the newest request is observed");
        assert_eq!(pipeline.generation(), 2);
    }

    #[test]
    fn the_pipeline_stops_running_in_the_poll_that_delivers_the_terminal_message() {
        let mut pipeline = Pipeline::new();
        let (job, _held) = ca_ui::testing::job_held_after(vec![
            TableMessage::Progress(Stage::Comparing),
            TableMessage::Cancelled,
        ]);
        pipeline.start(job);
        let messages = pipeline.poll();
        assert!(matches!(
            messages.as_slice(),
            [TableMessage::Progress(_), TableMessage::Cancelled]
        ));
        assert!(
            !pipeline.is_running(),
            "the pipeline still runs a job that has answered"
        );
    }

    #[test]
    fn stopping_the_pipeline_leaves_nothing_running() {
        let (_dir, left, right) = fixture("a\n1\n", "a\n2\n");
        let mut pipeline = Pipeline::new();
        pipeline.start(spawn_load(
            left,
            right,
            TableSettings::default(),
            Arc::new(|| {}),
        ));
        pipeline.cancel();
        pipeline.stop();
        assert!(!pipeline.is_running());
        assert!(pipeline.poll().is_empty());
    }

    #[test]
    fn a_date_tolerance_stops_a_close_instant_from_reading_as_different() {
        let (_dir, left, right) = fixture(
            "id,when\n1,01/02/2020 00:00\n",
            "id,when\n1,01/02/2020 00:30\n",
        );
        let mut strict_settings = TableSettings::default();
        strict_settings.schema.handling.insert(
            1,
            ColumnHandling {
                column_type: ColumnType::DateTime,
                use_default: false,
                ..ColumnHandling::default()
            },
        );
        let strict = run(&left, &right, strict_settings);
        assert_eq!(strict.comparison.totals.different, 1);

        let mut tolerant_settings = TableSettings::default();
        tolerant_settings.schema.handling.insert(
            1,
            ColumnHandling {
                column_type: ColumnType::DateTime,
                use_default: false,
                date_tolerance_seconds: 3_600.0,
                ..ColumnHandling::default()
            },
        );
        let tolerant = run(&left, &right, tolerant_settings);
        assert_eq!(tolerant.comparison.totals.different, 0);
        assert_eq!(tolerant.comparison.totals.unimportant, 1);
    }
}
