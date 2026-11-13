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
use scraper::{ElementRef, Html, Node, Selector};
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
    let Some(left_sheets) = read_sheets_for_comparison(
        left,
        &settings.left_format,
        clipboard[0].as_deref(),
        cancel,
        emitter,
    ) else {
        return;
    };
    if cancel.is_cancelled() {
        return;
    }
    let Some(right_sheets) = read_sheets_for_comparison(
        right,
        &settings.right_format,
        clipboard[1].as_deref(),
        cancel,
        emitter,
    ) else {
        return;
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
    let show_sheet_tabs = (is_sheeted_path(left) && clipboard[0].is_none())
        || (is_sheeted_path(right) && clipboard[1].is_none());
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
    pairing_key: [u8; 32],
    side: Side,
    text: Option<Arc<String>>,
}

struct RawSheet {
    name: String,
    pairing_key: Option<[u8; 32]>,
    text: String,
    encoding: String,
    had_errors: bool,
    stamp: Option<Stamp>,
    template: Option<Arc<LoadedText>>,
    kind: RawKind,
}

#[derive(Debug, Clone, Copy)]
enum RawKind {
    Text,
    Workbook,
    HtmlTable { first_row_is_header: bool },
}

struct PreparedSheet {
    raw: RawSheet,
    options: ParseOptions,
}

const MAX_SHEETS: usize = 256;
const MAX_SHEET_TEXT_BYTES: usize = ca_table::parse::MAX_INPUT_BYTES;
const MAX_HTML_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HTML_MARKUP_CHARACTERS: usize = 100_000;
const MAX_HTML_ATTRIBUTES_PER_TAG: usize = 128;
const MAX_HTML_DOCUMENT_ATTRIBUTES: usize = 200_000;
const MAX_HTML_TABLE_NAME_CHARS: usize = 120;
const MAX_HTML_DOCUMENT_CELLS: usize = ca_table::parse::MAX_TABLE_CELLS;
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

pub(crate) fn is_html_tables_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "asp" | "ascx" | "aspx" | "htm" | "html"
            )
        })
}

fn is_sheeted_path(path: &Path) -> bool {
    is_workbook_path(path) || is_html_tables_path(path)
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
    fn sheets_metadata(&self) -> &[calamine::Sheet] {
        match self {
            Self::Xlsx(reader) => reader.sheets_metadata(),
            Self::Xlsb(reader) => reader.sheets_metadata(),
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
    cancel: &Cancel,
) -> Result<Vec<RawSheet>, String> {
    if cancel.is_cancelled() {
        return Err(String::new());
    }
    if clipboard.is_none() && is_workbook_path(path) {
        return read_workbook_sheets(path);
    }
    if clipboard.is_none() && is_html_tables_path(path) {
        return read_html_tables(path, format, cancel);
    }
    let input = read_input(path, format, clipboard)?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Ok(vec![RawSheet {
        name,
        pairing_key: None,
        text: input.text,
        encoding: input.encoding,
        had_errors: input.had_errors,
        stamp: input.stamp,
        template: Some(input.template),
        kind: RawKind::Text,
    }])
}

fn read_sheets_for_comparison(
    path: &Path,
    format: &FormatSettings,
    clipboard: Option<&str>,
    cancel: &Cancel,
    emitter: &Emitter<TableMessage>,
) -> Option<Vec<RawSheet>> {
    match read_sheets(path, format, clipboard, cancel) {
        Ok(sheets) => Some(sheets),
        Err(message) => {
            if !cancel.is_cancelled() {
                emitter.send(TableMessage::Failed(message));
            }
            None
        }
    }
}

fn read_html_tables(
    path: &Path,
    format: &FormatSettings,
    cancel: &Cancel,
) -> Result<Vec<RawSheet>, String> {
    let input = read_html_input(path, format, cancel)?;
    check_html_markup_limit(&input.text, path, cancel)?;
    if cancel.is_cancelled() {
        return Err(String::new());
    }
    let document = Html::parse_document(&input.text);
    if cancel.is_cancelled() {
        return Err(String::new());
    }
    let selector = Selector::parse("table")
        .map_err(|_| "Table Compare could not create its HTML table selector.".to_owned())?;
    let mut tables = Vec::new();
    for table in document.select(&selector) {
        if cancel.is_cancelled() {
            return Err(String::new());
        }
        if tables.len() == MAX_SHEETS {
            return Err(format!(
                "{} has more than the {} HTML table limit",
                path.display(),
                MAX_SHEETS
            ));
        }
        tables.push(table);
    }
    if tables.is_empty() {
        return Err(format!(
            "{} contains no HTML tables that Table Compare can read",
            path.display()
        ));
    }
    let mut total_bytes = 0usize;
    let mut total_cells = 0usize;
    let mut sheets = Vec::with_capacity(tables.len());
    let mut pairing_keys = Vec::with_capacity(tables.len());
    let mut base_names = Vec::with_capacity(tables.len());
    for (index, table) in tables.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(String::new());
        }
        let full_name = html_table_name(*table, index + 1);
        pairing_keys.push(sheet_pairing_key(&full_name));
        base_names.push(bounded_html_table_name(&full_name));
    }
    let reserved_names = base_names
        .iter()
        .map(|name| normalized_sheet_name(name))
        .collect::<std::collections::HashSet<_>>();
    let mut name_counts = std::collections::HashMap::new();
    let mut assigned_names = std::collections::HashSet::new();
    for ((table, pairing_key), base_name) in tables.into_iter().zip(pairing_keys).zip(base_names) {
        if cancel.is_cancelled() {
            return Err(String::new());
        }
        let occurrence = name_counts.entry(pairing_key).or_insert(0usize);
        *occurrence += 1;
        let base_key = normalized_sheet_name(&base_name);
        let name = if *occurrence == 1 && !assigned_names.contains(&base_key) {
            base_name
        } else {
            let mut suffix = (*occurrence).max(2);
            loop {
                let candidate = format!("{base_name} ({suffix})");
                let candidate_key = normalized_sheet_name(&candidate);
                if !reserved_names.contains(&candidate_key)
                    && !assigned_names.contains(&candidate_key)
                {
                    break candidate;
                }
                suffix = suffix.saturating_add(1);
            }
        };
        assigned_names.insert(normalized_sheet_name(&name));
        let (text, first_row_is_header) = html_table_text(
            table,
            &name,
            path,
            &mut total_bytes,
            &mut total_cells,
            cancel,
        )?;
        sheets.push(RawSheet {
            name,
            pairing_key: Some(pairing_key),
            text,
            encoding: input.encoding.clone(),
            had_errors: input.had_errors,
            stamp: input.stamp,
            template: None,
            kind: RawKind::HtmlTable {
                first_row_is_header,
            },
        });
    }
    Ok(sheets)
}

fn html_table_name(table: ElementRef<'_>, ordinal: usize) -> String {
    let caption = table
        .child_elements()
        .find(|element| element.value().name() == "caption")
        .map(html_element_text)
        .filter(|text| !text.is_empty());
    caption
        .or_else(|| table.attr("aria-label").map(str::trim).map(str::to_owned))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| format!("Table {ordinal}"))
}

fn bounded_html_table_name(name: &str) -> String {
    let Some((end, _)) = name.char_indices().nth(MAX_HTML_TABLE_NAME_CHARS) else {
        return name.to_owned();
    };
    format!("{}…", &name[..end])
}

fn html_row_is_column_header(row: ElementRef<'_>) -> bool {
    let mut saw_header = false;
    for cell in row
        .child_elements()
        .filter(|element| matches!(element.value().name(), "td" | "th"))
    {
        if cell.value().name() != "th"
            || cell.attr("scope").is_some_and(|scope| {
                matches!(
                    scope.trim().to_ascii_lowercase().as_str(),
                    "row" | "rowgroup"
                )
            })
        {
            return false;
        }
        saw_header = true;
    }
    saw_header
}

fn check_html_markup_limit(text: &str, path: &Path, cancel: &Cancel) -> Result<(), String> {
    let mut markup_characters = 0usize;
    for (index, byte) in text.bytes().enumerate() {
        if index.is_multiple_of(65_536) && cancel.is_cancelled() {
            return Err(String::new());
        }
        markup_characters = markup_characters.saturating_add(usize::from(byte == b'<'));
        if markup_characters > MAX_HTML_MARKUP_CHARACTERS {
            break;
        }
    }
    if markup_characters > MAX_HTML_MARKUP_CHARACTERS {
        return Err(format!(
            "{} has more than the {MAX_HTML_MARKUP_CHARACTERS} HTML markup character limit",
            path.display()
        ));
    }
    let bytes = text.as_bytes();
    let mut index = 0usize;
    let mut total_attributes = 0usize;
    let mut attribute_scan_bytes = 0usize;
    let attribute_scan_limit = bytes.len().saturating_mul(2);
    while index + 1 < bytes.len() {
        if index.is_multiple_of(65_536) && cancel.is_cancelled() {
            return Err(String::new());
        }
        let is_end_tag = bytes[index] == b'<' && bytes[index + 1] == b'/';
        let name_start = index + usize::from(is_end_tag) + 1;
        if bytes[index] == b'<' && bytes.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            let Some((end, attributes)) = html_tag_attributes(bytes, name_start, cancel) else {
                return Err(String::new());
            };
            attribute_scan_bytes = attribute_scan_bytes.saturating_add(end.saturating_sub(index));
            if attribute_scan_bytes > attribute_scan_limit {
                return Err(format!(
                    "{} expands past the bounded HTML attribute scan limit",
                    path.display()
                ));
            }
            if attributes > MAX_HTML_ATTRIBUTES_PER_TAG {
                return Err(format!(
                    "{} has an HTML tag above the {MAX_HTML_ATTRIBUTES_PER_TAG} attribute limit",
                    path.display()
                ));
            }
            total_attributes = total_attributes.saturating_add(attributes);
            if total_attributes > MAX_HTML_DOCUMENT_ATTRIBUTES {
                return Err(format!(
                    "{} has more than the {MAX_HTML_DOCUMENT_ATTRIBUTES} HTML attribute limit",
                    path.display()
                ));
            }
        }
        index += 1;
    }
    Ok(())
}

fn html_tag_attributes(bytes: &[u8], name_start: usize, cancel: &Cancel) -> Option<(usize, usize)> {
    let mut index = name_start;
    while bytes
        .get(index)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'>'))
    {
        index += 1;
        if index.is_multiple_of(65_536) && cancel.is_cancelled() {
            return None;
        }
    }
    let mut attributes = 0usize;
    loop {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
            if index.is_multiple_of(65_536) && cancel.is_cancelled() {
                return None;
            }
        }
        match (bytes.get(index), bytes.get(index + 1)) {
            (Some(b'>'), _) => return Some((index + 1, attributes)),
            (Some(b'/'), Some(b'>')) => return Some((index + 2, attributes)),
            (Some(b'/'), _) => {
                index += 1;
                continue;
            }
            (None, _) => return Some((index, attributes)),
            _ => {}
        }
        attributes = attributes.saturating_add(1);
        if bytes.get(index) == Some(&b'=') {
            index += 1;
        }
        while bytes.get(index).is_some_and(|byte| {
            !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'>') && *byte != b'='
        }) {
            index += 1;
            if index.is_multiple_of(65_536) && cancel.is_cancelled() {
                return None;
            }
        }
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
            if index.is_multiple_of(65_536) && cancel.is_cancelled() {
                return None;
            }
        }
        if bytes.get(index) == Some(&b'/') {
            continue;
        }
        if bytes.get(index) == Some(&b'=') {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
                if index.is_multiple_of(65_536) && cancel.is_cancelled() {
                    return None;
                }
            }
            if let Some(quote @ (b'\'' | b'"')) = bytes.get(index).copied() {
                index += 1;
                while bytes.get(index).is_some_and(|byte| *byte != quote) {
                    index += 1;
                    if index.is_multiple_of(65_536) && cancel.is_cancelled() {
                        return None;
                    }
                }
                if bytes.get(index) == Some(&quote) {
                    index += 1;
                }
            } else {
                while bytes
                    .get(index)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
                {
                    index += 1;
                    if index.is_multiple_of(65_536) && cancel.is_cancelled() {
                        return None;
                    }
                }
            }
        }
    }
}

fn html_table_text(
    table: ElementRef<'_>,
    table_name: &str,
    path: &Path,
    total_bytes: &mut usize,
    total_document_cells: &mut usize,
    cancel: &Cancel,
) -> Result<(String, bool), String> {
    let mut builder =
        HtmlTableText::new(path, table_name, total_bytes, total_document_cells, cancel);
    for child in table.child_elements() {
        match child.value().name() {
            "tr" => builder.append_row(child, false)?,
            "thead" | "tbody" | "tfoot" => {
                builder.clear_row_group();
                let in_header = child.value().name() == "thead";
                for row in child.child_elements() {
                    if row.value().name() == "tr" {
                        builder.append_row(row, in_header)?;
                    }
                }
                builder.clear_row_group();
            }
            _ => {}
        }
    }
    Ok(builder.finish())
}

struct HtmlTableText<'a> {
    path: &'a Path,
    name: &'a str,
    total_bytes: &'a mut usize,
    total_document_cells: &'a mut usize,
    cancel: &'a Cancel,
    output: String,
    row_count: usize,
    total_cells: usize,
    row_spans: Vec<usize>,
    first_row_is_header: bool,
}

impl<'a> HtmlTableText<'a> {
    fn new(
        path: &'a Path,
        name: &'a str,
        total_bytes: &'a mut usize,
        total_document_cells: &'a mut usize,
        cancel: &'a Cancel,
    ) -> Self {
        Self {
            path,
            name,
            total_bytes,
            total_document_cells,
            cancel,
            output: String::new(),
            row_count: 0,
            total_cells: 0,
            row_spans: Vec::new(),
            first_row_is_header: false,
        }
    }

    fn clear_row_group(&mut self) {
        self.row_spans.clear();
    }

    fn append_row(&mut self, row: ElementRef<'_>, in_header: bool) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            return Err(String::new());
        }
        if self.row_count >= ca_table::parse::MAX_TABLE_ROWS {
            return Err(format!(
                "{} — {} is past the {} row HTML table limit",
                self.path.display(),
                self.name,
                ca_table::parse::MAX_TABLE_ROWS
            ));
        }
        if self.row_count == 0 {
            self.first_row_is_header = in_header || html_row_is_column_header(row);
        } else {
            self.append_delimiter('\n')?;
        }

        let values = self.row_values(row)?;
        if values.is_empty() {
            self.append_field("")?;
        }
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                self.append_delimiter('\t')?;
            }
            self.append_field(value)?;
        }
        self.row_count += 1;
        Ok(())
    }

    fn row_values(&mut self, row: ElementRef<'_>) -> Result<Vec<String>, String> {
        let occupied = self.row_spans.clone();
        let active_width = occupied
            .iter()
            .rposition(|remaining| *remaining > 0)
            .map_or(0, |column| column + 1);
        if self.total_cells.saturating_add(active_width) > ca_table::parse::MAX_TABLE_CELLS {
            return Err(html_table_cell_limit_error(self.path, self.name));
        }
        if self.total_document_cells.saturating_add(active_width) > MAX_HTML_DOCUMENT_CELLS {
            return Err(html_document_cell_limit_error(self.path));
        }
        let mut values = vec![String::new(); active_width];
        for remaining in &mut self.row_spans {
            *remaining = remaining.saturating_sub(1);
        }

        let mut column = 0usize;
        for (cell_index, cell) in row
            .child_elements()
            .filter(|element| matches!(element.value().name(), "td" | "th"))
            .enumerate()
        {
            if cell_index % 128 == 0 && self.cancel.is_cancelled() {
                return Err(String::new());
            }
            let colspan = html_span(
                cell,
                "colspan",
                ca_table::parse::MAX_TABLE_CELLS,
                self.path,
                self.name,
            )?;
            let rowspan = html_span(
                cell,
                "rowspan",
                ca_table::parse::MAX_TABLE_ROWS,
                self.path,
                self.name,
            )?;
            while let Some(offset) = occupied
                .iter()
                .skip(column)
                .take(colspan)
                .position(|remaining| *remaining > 0)
            {
                column = column.saturating_add(offset).saturating_add(1);
            }
            let end = column.saturating_add(colspan);
            if end > ca_table::parse::MAX_TABLE_CELLS {
                return Err(html_table_cell_limit_error(self.path, self.name));
            }
            let width = end.max(values.len());
            if self.total_cells.saturating_add(width) > ca_table::parse::MAX_TABLE_CELLS {
                return Err(html_table_cell_limit_error(self.path, self.name));
            }
            if self.total_document_cells.saturating_add(width) > MAX_HTML_DOCUMENT_CELLS {
                return Err(html_document_cell_limit_error(self.path));
            }
            values.resize(width, String::new());
            values[column] = html_element_text(cell);
            if rowspan > 1 {
                self.row_spans.resize(width, 0);
                for remaining in self.row_spans.iter_mut().skip(column).take(colspan) {
                    *remaining = (*remaining).max(rowspan.saturating_sub(1));
                }
            }
            column = end;
        }

        self.total_cells = self.total_cells.saturating_add(values.len());
        *self.total_document_cells = self.total_document_cells.saturating_add(values.len());
        if self.total_cells > ca_table::parse::MAX_TABLE_CELLS {
            return Err(html_table_cell_limit_error(self.path, self.name));
        }
        if *self.total_document_cells > MAX_HTML_DOCUMENT_CELLS {
            return Err(html_document_cell_limit_error(self.path));
        }
        while self
            .row_spans
            .last()
            .is_some_and(|remaining| *remaining == 0)
        {
            self.row_spans.pop();
        }
        Ok(values)
    }

    fn append_delimiter(&mut self, delimiter: char) -> Result<(), String> {
        if self
            .total_bytes
            .saturating_add(self.output.len())
            .saturating_add(1)
            > MAX_SHEET_TEXT_BYTES
        {
            return Err(html_table_text_limit_error(self.path, self.name));
        }
        self.output.push(delimiter);
        Ok(())
    }

    fn append_field(&mut self, value: &str) -> Result<(), String> {
        if self
            .total_bytes
            .saturating_add(self.output.len())
            .saturating_add(html_encoded_tab_field_len(value))
            > MAX_SHEET_TEXT_BYTES
        {
            return Err(html_table_text_limit_error(self.path, self.name));
        }
        append_html_encoded_tab_field(&mut self.output, value);
        Ok(())
    }

    fn finish(self) -> (String, bool) {
        *self.total_bytes = self.total_bytes.saturating_add(self.output.len());
        (self.output, self.first_row_is_header && self.row_count > 1)
    }
}

fn html_table_cell_limit_error(path: &Path, table_name: &str) -> String {
    format!(
        "{} — {table_name} is past the {} cell HTML table limit",
        path.display(),
        ca_table::parse::MAX_TABLE_CELLS
    )
}

fn html_document_cell_limit_error(path: &Path) -> String {
    format!(
        "{} is past the {MAX_HTML_DOCUMENT_CELLS} expanded HTML cell limit",
        path.display()
    )
}

fn html_span(
    cell: ElementRef<'_>,
    attribute: &str,
    maximum: usize,
    path: &Path,
    table_name: &str,
) -> Result<usize, String> {
    let parsed = cell
        .attr(attribute)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(1);
    let span = if attribute == "rowspan" && parsed == 0 {
        maximum
    } else if parsed == 0 {
        1
    } else {
        parsed
    };
    if span > maximum {
        return Err(format!(
            "{} — {table_name} has an HTML {attribute} above its {maximum} limit",
            path.display()
        ));
    }
    Ok(span)
}

fn html_table_text_limit_error(path: &Path, table_name: &str) -> String {
    format!(
        "{} — {table_name} expands past the {} byte HTML table limit",
        path.display(),
        MAX_SHEET_TEXT_BYTES
    )
}

fn html_element_text(element: ElementRef<'_>) -> String {
    let mut output = String::new();
    let mut pending = element.children().map(Some).collect::<Vec<_>>();
    pending.reverse();
    while let Some(node) = pending.pop() {
        let Some(node) = node else {
            output.push(' ');
            continue;
        };
        match node.value() {
            Node::Text(text) => output.push_str(&text.text),
            Node::Element(tag) => {
                if matches!(tag.name(), "table" | "script" | "style" | "noscript") {
                    continue;
                }
                let is_block = is_html_block_element(tag.name());
                if tag.name() == "br" || is_block {
                    output.push(' ');
                }
                if is_block {
                    pending.push(None);
                }
                let mut children = node.children().collect::<Vec<_>>();
                children.reverse();
                pending.extend(children.into_iter().map(Some));
            }
            _ => {}
        }
    }
    let mut normalized = String::new();
    for word in output.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

fn is_html_block_element(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "dd"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "ul"
    )
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
            let options = match raw.kind {
                RawKind::Text => resolve(&raw.text, format, settings, regional),
                RawKind::Workbook => ParseOptions {
                    syntax: ca_table::parse::FieldSyntax::delimited(['\t']),
                    ..ParseOptions::default()
                },
                RawKind::HtmlTable {
                    first_row_is_header,
                } => ParseOptions {
                    syntax: ca_table::parse::FieldSyntax::delimited(['\t']),
                    first_line_contains: if first_row_is_header {
                        ca_table::parse::FirstLineContains::ColumnNames
                    } else {
                        ca_table::parse::FirstLineContains::CellData
                    },
                    ..ParseOptions::default()
                },
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
                syntax: match raw.kind {
                    RawKind::Text => syntax_label(&resolved_options),
                    RawKind::Workbook => "Spreadsheet cells".to_owned(),
                    RawKind::HtmlTable { .. } => "HTML table cells".to_owned(),
                },
                header: header_label(&table),
                warnings: table.warnings().to_vec(),
                options: resolved_options,
                stamp: raw.stamp,
                template: raw.template,
            };
            let pairing_key = raw
                .pairing_key
                .unwrap_or_else(|| sheet_pairing_key(&raw.name));
            Ok(LoadedSheet {
                name: raw.name,
                pairing_key,
                side: Side {
                    table: Arc::new(table),
                    facts,
                },
                text: matches!(raw.kind, RawKind::Text).then(|| Arc::new(raw.text)),
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
    let metadata = workbook.sheets_metadata();
    let sheet_count = metadata.len();
    if sheet_count > MAX_SHEETS {
        return Err(format!(
            "{} has {sheet_count} sheets, past the {MAX_SHEETS} sheet limit",
            path.display(),
        ));
    }
    let names: Vec<_> = metadata
        .iter()
        .filter(|sheet| sheet.typ == calamine::SheetType::WorkSheet)
        .map(|sheet| sheet.name.clone())
        .collect();
    if names.is_empty() {
        return Err(format!(
            "{} contains no worksheets that Table Compare can read",
            path.display()
        ));
    }
    let mut sheets = Vec::with_capacity(names.len());
    let mut total_bytes = 0usize;
    for name in names {
        let text = read_streaming_sheet(&mut workbook, &name, path, &mut total_bytes)?;
        sheets.push(RawSheet {
            name,
            pairing_key: None,
            text,
            encoding: "Spreadsheet values".to_owned(),
            had_errors: false,
            stamp,
            template: None,
            kind: RawKind::Workbook,
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
    if output.len() > MAX_SHEET_TEXT_BYTES
        || total_bytes.saturating_add(output.len()) > MAX_SHEET_TEXT_BYTES
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
    let encoded_len = encoded_tab_field_len(value);
    let quoted = value
        .chars()
        .any(|character| matches!(character, '\t' | '\n' | '\r' | '"'))
        || value.trim() != value;
    if previous_bytes
        .saturating_add(output.len())
        .saturating_add(encoded_len)
        > MAX_SHEET_TEXT_BYTES
    {
        return Err(workbook_text_limit_error(path, sheet));
    }
    append_encoded_tab_field_with_quote(output, value, quoted);
    Ok(())
}

fn encoded_tab_field_len(value: &str) -> usize {
    let quoted = value
        .chars()
        .any(|character| matches!(character, '\t' | '\n' | '\r' | '"'))
        || value.trim() != value;
    value
        .len()
        .saturating_add(if quoted { 2 } else { 0 })
        .saturating_add(if quoted {
            value.chars().filter(|character| *character == '"').count()
        } else {
            0
        })
}

fn html_encoded_tab_field_len(value: &str) -> usize {
    if value.is_empty() {
        2
    } else {
        encoded_tab_field_len(value)
    }
}

fn append_encoded_tab_field(output: &mut String, value: &str) {
    let quoted = value
        .chars()
        .any(|character| matches!(character, '\t' | '\n' | '\r' | '"'))
        || value.trim() != value;
    append_encoded_tab_field_with_quote(output, value, quoted);
}

fn append_html_encoded_tab_field(output: &mut String, value: &str) {
    if value.is_empty() {
        append_encoded_tab_field_with_quote(output, value, true);
    } else {
        append_encoded_tab_field(output, value);
    }
}

fn append_encoded_tab_field_with_quote(output: &mut String, value: &str, quoted: bool) {
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
}

fn workbook_text_limit_error(path: &Path, sheet: &str) -> String {
    format!(
        "{} — {sheet} expands past the {} byte workbook table limit",
        path.display(),
        MAX_SHEET_TEXT_BYTES
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
                    (!used[index] && sheet.pairing_key == candidate.pairing_key).then_some(index)
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
                    (!used[index] && sheet.pairing_key == candidate.pairing_key).then_some(index)
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

fn sheet_pairing_key(name: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for character in name
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
    {
        let mut buffer = [0; 4];
        hasher.update(character.encode_utf8(&mut buffer).as_bytes());
    }
    *hasher.finalize().as_bytes()
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

#[derive(Clone, Copy)]
enum ReadMode<'a> {
    Table,
    Html(&'a Cancel),
}

fn read_side_with_open<R: Read>(
    path: &Path,
    format: &FormatSettings,
    file_size: u64,
    open: impl FnOnce() -> std::io::Result<R>,
) -> Result<SideText, String> {
    read_side_with_open_mode(path, format, file_size, open, ReadMode::Table)
}

fn read_html_input(
    path: &Path,
    format: &FormatSettings,
    cancel: &Cancel,
) -> Result<SideText, String> {
    if cancel.is_cancelled() {
        return Err(String::new());
    }
    let file_size = std::fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    read_side_with_open_mode(
        path,
        format,
        file_size,
        || std::fs::File::open(path),
        ReadMode::Html(cancel),
    )
}

fn read_side_with_open_mode<R: Read>(
    path: &Path,
    format: &FormatSettings,
    file_size: u64,
    open: impl FnOnce() -> std::io::Result<R>,
    mode: ReadMode<'_>,
) -> Result<SideText, String> {
    let (sniff_html_charset, input_limit, input_kind, cancel) = match mode {
        ReadMode::Table => (false, ca_table::parse::MAX_INPUT_BYTES, "table", None),
        ReadMode::Html(cancel) => (true, MAX_HTML_INPUT_BYTES, "HTML", Some(cancel)),
    };
    if cancel.is_some_and(Cancel::is_cancelled) {
        return Err(String::new());
    }
    let input_limit_bytes = u64::try_from(input_limit).unwrap_or(u64::MAX);
    if file_size > input_limit_bytes {
        return Err(format!(
            "{} is {file_size} bytes, past the {input_limit_bytes} byte {input_kind} input limit",
            path.display()
        ));
    }
    // The stamp is taken before the read, so a change made while the bytes are
    // read is reported by the next save rather than overwritten.
    let stamp = RealFileSystem.stamp(path);
    let mut file = open().map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(file_size)
            .unwrap_or(input_limit)
            .min(input_limit),
    );
    let mut chunk = [0; 16 * 1024];
    loop {
        if cancel.is_some_and(Cancel::is_cancelled) {
            return Err(String::new());
        }
        let remaining_plus_one = input_limit.saturating_sub(bytes.len()).saturating_add(1);
        let read_size = chunk.len().min(remaining_plus_one);
        let count = match file.read(&mut chunk[..read_size]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > input_limit {
            return Err(format!(
                "{} grew past the {input_limit_bytes} byte {input_kind} input limit while it was read",
                path.display()
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if cancel.is_some_and(Cancel::is_cancelled) {
        return Err(String::new());
    }
    let declared_html_encoding =
        (sniff_html_charset && format.encoding.is_none() && !has_byte_order_mark(&bytes))
            .then(|| html_meta_encoding(&bytes))
            .flatten();
    let options = DecodeOptions {
        forced: format.encoding.or(declared_html_encoding),
        ..DecodeOptions::default()
    };
    let mut loaded = LoadedText::load(&bytes, &options);
    if cancel.is_some_and(Cancel::is_cancelled) {
        return Err(String::new());
    }
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

fn has_byte_order_mark(bytes: &[u8]) -> bool {
    [
        &[0x00, 0x00, 0xFE, 0xFF][..],
        &[0xFF, 0xFE, 0x00, 0x00][..],
        &[0xEF, 0xBB, 0xBF][..],
        &[0xFE, 0xFF][..],
        &[0xFF, 0xFE][..],
    ]
    .iter()
    .any(|bom| bytes.starts_with(bom))
}

fn html_meta_encoding(bytes: &[u8]) -> Option<ca_text::TextEncoding> {
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
    let document = Html::parse_document(&prefix);
    let selector = Selector::parse("meta").ok()?;
    for meta in document.select(&selector) {
        let label = meta
            .attr("charset")
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                meta.attr("http-equiv")
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("content-type"))
                    .then(|| meta.attr("content").and_then(html_content_charset))
                    .flatten()
            });
        if let Some(encoding) = label.as_deref().and_then(ca_text::TextEncoding::from_label) {
            return Some(match encoding {
                ca_text::TextEncoding::Utf16Le | ca_text::TextEncoding::Utf16Be => {
                    ca_text::TextEncoding::Utf8
                }
                other => other,
            });
        }
    }
    None
}

fn html_content_charset(content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(offset) = lower[search_from..].find("charset") {
        let start = search_from + offset;
        let before_ok = start == 0
            || !lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                && lower.as_bytes()[start - 1] != b'-';
        let after_key = start.saturating_add("charset".len());
        let rest = lower[after_key..].trim_start();
        if before_ok && rest.starts_with('=') {
            let value_start = after_key + lower[after_key..].len() - rest.len() + 1;
            let value = content[value_start..].trim_start();
            let value = value.trim_matches(|character: char| character == '\'' || character == '"');
            let label = value
                .chars()
                .take_while(|character| {
                    !character.is_ascii_whitespace()
                        && *character != ';'
                        && *character != '\''
                        && *character != '"'
                })
                .collect::<String>();
            if !label.is_empty() {
                return Some(label);
            }
        }
        search_from = after_key;
    }
    None
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
        read_html_input, read_html_tables, read_side_with_open, read_side_with_open_mode,
        read_workbook_sheets, spawn_load, spawn_recompare, Cancel, FormatSettings, HtmlTableText,
        Pipeline, ReadMode, Side, Stage, TableMessage, TableSettings, MAX_HTML_ATTRIBUTES_PER_TAG,
        MAX_HTML_DOCUMENT_CELLS, MAX_HTML_INPUT_BYTES, MAX_HTML_MARKUP_CHARACTERS,
        MAX_HTML_TABLE_NAME_CHARS,
    };
    use ca_table::compare::CellStatus;
    use ca_table::schema::{ColumnAlignment, ColumnHandling, ColumnType, ManualColumnPair};
    use scraper::{Html, Selector};
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
    fn an_html_file_over_its_input_limit_is_refused_before_reading_it() {
        let path = Path::new("oversized.html");
        let mut opened = false;
        let cancel = Cancel::new();
        let result = read_side_with_open_mode(
            path,
            &FormatSettings::default(),
            u64::try_from(MAX_HTML_INPUT_BYTES).unwrap() + 1,
            || {
                opened = true;
                Ok(std::io::Cursor::new(b"<table></table>".to_vec()))
            },
            ReadMode::Html(&cancel),
        );

        assert!(matches!(result, Err(error) if error.contains("HTML input limit")));
        assert!(!opened, "an oversized HTML file must not be opened");
    }

    #[test]
    fn html_input_read_stops_when_cancelled() {
        struct CancelOnRead {
            cancel: Cancel,
            read: bool,
        }

        impl IoRead for CancelOnRead {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.read || buffer.is_empty() {
                    return Ok(0);
                }
                self.read = true;
                let source = b"<table></table>";
                buffer[..source.len()].copy_from_slice(source);
                self.cancel.cancel();
                Ok(source.len())
            }
        }

        let cancel = Cancel::new();
        let result = read_side_with_open_mode(
            Path::new("cancelled.html"),
            &FormatSettings::default(),
            15,
            || {
                Ok(CancelOnRead {
                    cancel: cancel.clone(),
                    read: false,
                })
            },
            ReadMode::Html(&cancel),
        );

        assert!(matches!(result, Err(error) if error.is_empty()));
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

    fn workbook_with_chartsheet_bytes() -> Vec<u8> {
        let fixture = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/multiple-left.xlsx"
        ));
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(fixture.as_slice())).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().unwrap().into_owned();
            if name == "xl/worksheets/sheet3.xml" {
                continue;
            }
            let is_directory = entry.is_dir();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name == "xl/_rels/workbook.xml.rels" {
                let xml = String::from_utf8(bytes).unwrap();
                let worksheet_relation = "Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet3.xml\"";
                assert!(xml.contains(worksheet_relation));
                bytes = xml
                    .replace(
                        worksheet_relation,
                        "Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet\" Target=\"chartsheets/sheet3.xml\"",
                    )
                    .into_bytes();
            } else if name == "[Content_Types].xml" {
                let xml = String::from_utf8(bytes).unwrap();
                let worksheet_content_type = "PartName=\"/xl/worksheets/sheet3.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"";
                assert!(xml.contains(worksheet_content_type));
                let xml = xml
                    .replace(
                        worksheet_content_type,
                        "PartName=\"/xl/chartsheets/sheet3.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml\"",
                    );
                assert!(xml.contains("</Types>"));
                bytes = xml
                    .replacen(
                        "</Types>",
                        concat!(
                            r#"<Override PartName="/xl/drawings/drawing3.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/>"#,
                            r#"<Override PartName="/xl/charts/chart3.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>"#,
                            "</Types>"
                        ),
                        1,
                    )
                    .into_bytes();
            }
            let options = zip::write::SimpleFileOptions::default();
            if is_directory {
                writer.add_directory(name, options).unwrap();
            } else {
                writer.start_file(name, options).unwrap();
                writer.write_all(&bytes).unwrap();
            }
        }
        writer
            .start_file(
                "xl/chartsheets/sheet3.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer
            .write_all(
                br#"<chartsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheetViews><sheetView workbookViewId="0"/></sheetViews>
  <drawing r:id="rId1"/>
</chartsheet>"#,
            )
            .unwrap();
        for (name, xml) in chartsheet_parts() {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(xml.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn chartsheet_parts() -> [(&'static str, &'static str); 4] {
        [
            (
                "xl/chartsheets/_rels/sheet3.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing3.xml"/>
</Relationships>"#,
            ),
            (
                "xl/drawings/drawing3.xml",
                r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <xdr:absoluteAnchor>
    <xdr:pos x="0" y="0"/>
    <xdr:ext cx="9144000" cy="5143500"/>
    <xdr:graphicFrame macro="">
      <xdr:nvGraphicFramePr>
        <xdr:cNvPr id="2" name="Chart 1"/>
        <xdr:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></xdr:cNvGraphicFramePr>
      </xdr:nvGraphicFramePr>
      <xdr:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/></xdr:xfrm>
      <a:graphic>
        <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">
          <c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1"/>
        </a:graphicData>
      </a:graphic>
    </xdr:graphicFrame>
    <xdr:clientData/>
  </xdr:absoluteAnchor>
</xdr:wsDr>"#,
            ),
            (
                "xl/drawings/_rels/drawing3.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart3.xml"/>
</Relationships>"#,
            ),
            (
                "xl/charts/chart3.xml",
                r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">
  <c:chart>
    <c:plotArea>
      <c:layout/>
      <c:barChart>
        <c:barDir val="col"/>
        <c:grouping val="clustered"/>
        <c:ser>
          <c:idx val="0"/><c:order val="0"/>
          <c:tx><c:v>Series 1</c:v></c:tx>
          <c:cat><c:strLit><c:ptCount val="1"/><c:pt idx="0"><c:v>Sample</c:v></c:pt></c:strLit></c:cat>
          <c:val><c:numLit><c:formatCode>General</c:formatCode><c:ptCount val="1"/><c:pt idx="0"><c:v>1</c:v></c:pt></c:numLit></c:val>
        </c:ser>
        <c:axId val="1"/><c:axId val="2"/>
      </c:barChart>
      <c:catAx>
        <c:axId val="1"/><c:scaling><c:orientation val="minMax"/></c:scaling>
        <c:axPos val="b"/><c:tickLblPos val="nextTo"/><c:crossAx val="2"/>
        <c:crosses val="autoZero"/>
      </c:catAx>
      <c:valAx>
        <c:axId val="2"/><c:scaling><c:orientation val="minMax"/></c:scaling>
        <c:axPos val="l"/><c:crossAx val="1"/><c:crosses val="autoZero"/>
        <c:crossBetween val="between"/>
      </c:valAx>
    </c:plotArea>
    <c:legend><c:legendPos val="r"/></c:legend>
  </c:chart>
</c:chartSpace>"#,
            ),
        ]
    }

    fn workbook_with_only_chartsheets_bytes() -> Vec<u8> {
        let fixture = workbook_with_chartsheet_bytes();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(fixture)).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().unwrap().into_owned();
            if matches!(
                name.as_str(),
                "xl/worksheets/sheet1.xml" | "xl/worksheets/sheet2.xml"
            ) {
                continue;
            }
            let is_directory = entry.is_dir();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name == "xl/workbook.xml" {
                let xml = String::from_utf8(bytes).unwrap();
                let worksheet_entries = [
                    "<sheet name=\"One\" sheetId=\"1\" r:id=\"rId1\"/>",
                    "<sheet name=\"Shared A\" sheetId=\"2\" r:id=\"rId2\"/>",
                ];
                for worksheet in worksheet_entries {
                    assert!(xml.contains(worksheet));
                }
                bytes = worksheet_entries
                    .into_iter()
                    .fold(xml, |xml, worksheet| xml.replace(worksheet, ""))
                    .into_bytes();
            } else if name == "xl/_rels/workbook.xml.rels" {
                let xml = String::from_utf8(bytes).unwrap();
                let worksheet_relationships = [
                    "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>",
                    "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet2.xml\"/>",
                ];
                for relationship in worksheet_relationships {
                    assert!(xml.contains(relationship));
                }
                bytes = worksheet_relationships
                    .into_iter()
                    .fold(xml, |xml, relationship| xml.replace(relationship, ""))
                    .into_bytes();
            } else if name == "[Content_Types].xml" {
                let xml = String::from_utf8(bytes).unwrap();
                let worksheet_content_types = [
                    "<Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>",
                    "<Override PartName=\"/xl/worksheets/sheet2.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>",
                ];
                for content_type in worksheet_content_types {
                    assert!(xml.contains(content_type));
                }
                bytes = worksheet_content_types
                    .into_iter()
                    .fold(xml, |xml, content_type| xml.replace(content_type, ""))
                    .into_bytes();
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
    fn chartsheets_are_skipped_while_worksheets_continue_loading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("with-chartsheet.xlsx");
        std::fs::write(&path, workbook_with_chartsheet_bytes()).unwrap();

        let sheets = read_workbook_sheets(&path).unwrap();

        assert_eq!(
            sheets
                .iter()
                .map(|sheet| sheet.name.as_str())
                .collect::<Vec<_>>(),
            ["One", "Shared A"]
        );
        assert!(!sheets[0].text.is_empty());
        assert!(!sheets[1].text.is_empty());
    }

    #[test]
    fn workbook_with_no_worksheets_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chartsheet-only.xlsx");
        std::fs::write(&path, workbook_with_only_chartsheets_bytes()).unwrap();

        assert!(read_workbook_sheets(&path)
            .is_err_and(|error| error.contains("no worksheets that Table Compare can read")));
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
    fn html_tables_load_as_named_read_only_sheets_and_pair_by_caption() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.asp");
        let right = dir.path().join("right.html");
        std::fs::write(
            &left,
            concat!(
                "<table><caption>Summary</caption><thead><tr><th>ID</th><th>Value</th></tr></thead>",
                "<tbody><tr><td>1</td><td>Left &amp; one</td></tr></tbody></table>",
                "<table aria-label=\"Details\"><tr><th>Code</th><th>Text</th></tr>",
                "<tr><td>A</td><td>left detail</td></tr></table>"
            ),
        )
        .unwrap();
        std::fs::write(
            &right,
            concat!(
                "<table aria-label=\"Details\"><tr><th>Code</th><th>Text</th></tr>",
                "<tr><td>A</td><td>right detail</td></tr></table>",
                "<table><caption>Summary</caption><tr><th>ID</th><th>Value</th></tr>",
                "<tr><td>1</td><td>Right</td></tr></table>"
            ),
        )
        .unwrap();
        let mut settings = TableSettings::default();
        settings.sheets = ca_session::settings::table::TablePairing::by_left_name();

        let data = run(&left, &right, settings);

        assert!(data.show_sheet_tabs);
        assert_eq!(data.sheet_pairs.len(), 2);
        assert_eq!(data.sheet_pairs[0].label, "Summary");
        assert_eq!(data.sheet_pairs[0].right_index, Some(1));
        assert_eq!(data.sheet_pairs[1].label, "Details");
        assert_eq!(data.sheet_pairs[1].right_index, Some(0));
        assert_eq!(
            data.sheet_pairs[0].left.table.first_line_contains(),
            &ca_table::parse::FirstLineContains::ColumnNames
        );
        assert_eq!(data.sheet_pairs[0].left.table.cell_text(0, 1), "Left & one");
        assert_eq!(
            data.sheet_pairs[1].right.table.cell_text(0, 1),
            "right detail"
        );
        assert_eq!(data.sheet_pairs[0].left.facts.syntax, "HTML table cells");
        assert!(data.texts.is_none());
        assert!(data.sheet_pairs.iter().all(|sheet| {
            sheet.texts.is_none()
                && sheet.left.facts.template.is_none()
                && sheet.right.facts.template.is_none()
        }));
    }

    #[test]
    fn html_tables_do_not_flatten_nested_table_cells_into_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        std::fs::write(
            &left,
            "<table><tr><td>Before <table><tr><td>Nested</td></tr></table> after</td></tr></table>",
        )
        .unwrap();
        std::fs::write(
            &right,
            "<table><tr><td>Before after</td></tr></table><table><tr><td>Nested</td></tr></table>",
        )
        .unwrap();

        let data = run(&left, &right, TableSettings::default());

        assert_eq!(data.sheet_pairs.len(), 2);
        assert_eq!(
            data.sheet_pairs[0].left.table.cell_text(0, 0),
            "Before after"
        );
        assert_eq!(data.sheet_pairs[1].left.table.cell_text(0, 0), "Nested");
    }

    #[test]
    fn html_table_row_and_column_spans_keep_cell_positions() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        let html = concat!(
            "<table><tr><th colspan=\"2\">Group</th><th>Code</th></tr>",
            "<tr><td rowspan=\"2\">A</td><td>B</td><td>1</td></tr>",
            "<tr><td>C</td><td>2</td></tr></table>"
        );
        std::fs::write(&left, html).unwrap();
        std::fs::write(&right, html).unwrap();

        let data = run(&left, &right, TableSettings::default());

        assert_eq!(data.sheet_pairs[0].left.table.cell_text(1, 1), "C");
        assert_eq!(data.sheet_pairs[0].left.table.cell_text(1, 2), "2");
    }

    #[test]
    fn html_rowspan_zero_grows_to_the_end_of_its_row_group() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        let html = concat!(
            "<table><tbody><tr><td rowspan=\"0\">A</td><td>1</td></tr>",
            "<tr><td>X</td></tr></tbody></table>"
        );
        std::fs::write(&left, html).unwrap();
        std::fs::write(&right, html).unwrap();

        let data = run(&left, &right, TableSettings::default());

        assert_eq!(data.sheet_pairs[0].left.table.cell_text(1, 0), "");
        assert_eq!(data.sheet_pairs[0].left.table.cell_text(1, 1), "X");
    }

    #[test]
    fn html_row_spans_do_not_cross_row_group_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        let html = concat!(
            "<table><tbody><tr><td rowspan=\"2\">A</td><td>1</td></tr></tbody>",
            "<tbody><tr><td>B</td><td>2</td></tr></tbody></table>"
        );
        std::fs::write(&left, html).unwrap();
        std::fs::write(&right, html).unwrap();

        let data = run(&left, &right, TableSettings::default());

        assert_eq!(data.sheet_pairs[0].left.table.cell_text(1, 0), "B");
        assert_eq!(data.sheet_pairs[0].left.table.cell_text(1, 1), "2");
    }

    #[test]
    fn html_empty_cells_at_table_edges_are_retained() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        let html = concat!(
            "<table><tr><td></td></tr><tr><td>middle</td></tr>",
            "<tr><td></td></tr></table>"
        );
        std::fs::write(&left, html).unwrap();
        std::fs::write(&right, html).unwrap();

        let data = run(&left, &right, TableSettings::default());
        let table = &data.sheet_pairs[0].left.table;

        assert_eq!(table.row_count(), 3);
        assert_eq!(table.column_count(), 1);
        assert_eq!(table.cell_text(0, 0), "");
        assert_eq!(table.cell_text(2, 0), "");
    }

    #[test]
    fn html_row_headers_remain_data_and_differences_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        let left_html = concat!(
            "<table><tbody><tr><th scope=\"row\">Product</th><td>1</td></tr>",
            "<tr><th scope=\"row\">Other</th><td>2</td></tr></tbody></table>"
        );
        let right_html = concat!(
            "<table><tbody><tr><th scope=\"row\">Goods</th><td>1</td></tr>",
            "<tr><th scope=\"row\">Other</th><td>2</td></tr></tbody></table>"
        );
        std::fs::write(&left, left_html).unwrap();
        std::fs::write(&right, right_html).unwrap();

        let data = run(&left, &right, TableSettings::default());

        assert_eq!(data.sheet_pairs[0].left.table.row_count(), 2);
        assert_eq!(
            data.sheet_pairs[0].left.table.first_line_contains(),
            &ca_table::parse::FirstLineContains::CellData
        );
        assert_eq!(data.comparison.totals.rows_with_differences(), 1);
    }

    #[test]
    fn html_header_only_tables_keep_their_columns() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        std::fs::write(
            &left,
            "<table><thead><tr><th>Name</th><th>Value</th></tr></thead></table>",
        )
        .unwrap();
        std::fs::write(
            &right,
            "<table><thead><tr><th>Label</th><th>Value</th></tr></thead></table>",
        )
        .unwrap();

        let data = run(&left, &right, TableSettings::default());
        let table = &data.sheet_pairs[0].left.table;

        assert_eq!(table.row_count(), 1);
        assert_eq!(table.column_count(), 2);
        assert_eq!(table.cell_text(0, 0), "Name");
        assert_eq!(table.cell_text(0, 1), "Value");
        assert_eq!(data.comparison.totals.rows_with_differences(), 1);
    }

    #[test]
    fn html_headers_keep_columns_missing_from_every_data_row() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        std::fs::write(
            &left,
            concat!(
                "<table><thead><tr><th>A</th><th>B</th><th>C</th></tr></thead>",
                "<tbody><tr><td>1</td><td>2</td></tr></tbody></table>"
            ),
        )
        .unwrap();
        std::fs::write(
            &right,
            concat!(
                "<table><thead><tr><th>A</th><th>B</th><th>C</th></tr></thead>",
                "<tbody><tr><td>1</td><td>3</td></tr></tbody></table>"
            ),
        )
        .unwrap();

        let data = run(&left, &right, TableSettings::default());
        let table = &data.sheet_pairs[0].left.table;

        assert_eq!(table.column_count(), 3);
        assert_eq!(table.header().unwrap(), ["A", "B", "C"].map(str::to_owned));
        assert_eq!(table.row_count(), 1);
        assert_eq!(table.row_len(0), 2);
        assert_eq!(table.cell_text(0, 2), "");
    }

    #[test]
    fn html_duplicate_captions_do_not_collide_with_literal_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        std::fs::write(
            &left,
            html_named_tables(&[("Q", "left one"), ("Q", "left two"), ("Q (2)", "literal")]),
        )
        .unwrap();
        std::fs::write(
            &right,
            html_named_tables(&[("Q (2)", "literal"), ("Q", "right one"), ("Q", "right two")]),
        )
        .unwrap();
        let mut settings = TableSettings::default();
        settings.sheets = ca_session::settings::table::TablePairing::by_left_name();

        let data = run(&left, &right, settings);

        assert_eq!(data.sheet_pairs.len(), 3);
        assert_eq!(data.sheet_pairs[0].right_index, Some(1));
        assert_eq!(data.sheet_pairs[1].right_index, Some(2));
        assert_eq!(data.sheet_pairs[2].right_index, Some(0));
        assert_eq!(data.sheet_pairs[1].left_name.as_deref(), Some("Q (3)"));
        assert_eq!(data.sheet_pairs[2].left_name.as_deref(), Some("Q (2)"));
    }

    fn html_named_tables(tables: &[(&str, &str)]) -> String {
        tables
            .iter()
            .fold(String::new(), |mut html, (caption, value)| {
                write!(
                    html,
                    "<table><caption>{caption}</caption><tr><td>{value}</td></tr></table>"
                )
                .unwrap();
                html
            })
    }

    #[test]
    fn html_long_captions_keep_distinct_pairing_keys_and_bounded_labels() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        let prefix = "x".repeat(MAX_HTML_TABLE_NAME_CHARS + 40);
        let name_a = format!("{prefix}A");
        let name_b = format!("{prefix}B");
        std::fs::write(&left, html_named_tables(&[(&name_a, "A"), (&name_b, "B")])).unwrap();
        std::fs::write(&right, html_named_tables(&[(&name_b, "B"), (&name_a, "A")])).unwrap();
        let mut settings = TableSettings::default();
        settings.sheets = ca_session::settings::table::TablePairing::by_left_name();

        let data = run(&left, &right, settings);

        assert_eq!(data.sheet_pairs[0].right_index, Some(1));
        assert_eq!(data.sheet_pairs[1].right_index, Some(0));
        assert!(data
            .sheet_pairs
            .iter()
            .all(|pair| { pair.label.chars().count() <= 2 * (MAX_HTML_TABLE_NAME_CHARS + 5) + 3 }));
    }

    #[test]
    fn html_block_elements_separate_adjacent_visible_text() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.html");
        std::fs::write(
            &left,
            "<table><tr><td><div>A</div><div>B</div></td></tr></table>",
        )
        .unwrap();
        std::fs::write(&right, "<table><tr><td>AB</td></tr></table>").unwrap();

        let data = run(&left, &right, TableSettings::default());

        assert_eq!(data.sheet_pairs[0].left.table.cell_text(0, 0), "A B");
        assert_eq!(data.comparison.totals.rows_with_differences(), 1);
    }

    #[test]
    fn html_meta_charset_is_used_without_overriding_a_bom_or_user_choice() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy.html");
        let bom = dir.path().join("bom.html");
        let prefix = b"<meta charset=\"windows-1252\"><table><tr><td>";
        let suffix = b"</td></tr></table>";
        let mut legacy_bytes = prefix.to_vec();
        legacy_bytes.extend_from_slice(&[0xC3, 0xA9]);
        legacy_bytes.extend_from_slice(suffix);
        std::fs::write(&legacy, legacy_bytes).unwrap();

        let legacy_input =
            read_html_input(&legacy, &FormatSettings::default(), &Cancel::new()).unwrap();
        assert_eq!(legacy_input.encoding, "windows-1252");
        assert!(legacy_input.text.contains("Ã©"));

        let mut bom_bytes = vec![0xEF, 0xBB, 0xBF];
        bom_bytes.extend_from_slice(prefix);
        bom_bytes.extend_from_slice("é".as_bytes());
        bom_bytes.extend_from_slice(suffix);
        std::fs::write(&bom, bom_bytes).unwrap();
        let bom_input = read_html_input(&bom, &FormatSettings::default(), &Cancel::new()).unwrap();
        assert_eq!(bom_input.encoding, "UTF-8");
        assert!(bom_input.text.contains('é'));

        let override_input = read_html_input(
            &legacy,
            &FormatSettings {
                encoding: Some(ca_text::TextEncoding::Utf8),
                ..FormatSettings::default()
            },
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(override_input.encoding, "UTF-8");
        assert!(override_input.text.contains('é'));
    }

    #[test]
    fn html_markup_budget_rejects_input_before_building_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many-tags.html");
        let html = format!("<!-->{}", "<".repeat(MAX_HTML_MARKUP_CHARACTERS + 1));
        std::fs::write(&path, html).unwrap();

        let error = read_html_tables(&path, &FormatSettings::default(), &Cancel::new())
            .err()
            .expect("the markup budget rejects the document");

        assert!(error.contains("HTML markup character limit"));
    }

    #[test]
    fn html_attribute_budget_rejects_an_attribute_dense_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many-attributes.html");
        let end_tag_path = dir.path().join("many-end-tag-attributes.html");
        let mut attributes = String::new();
        for index in 0..=MAX_HTML_ATTRIBUTES_PER_TAG {
            write!(attributes, " data-{index}").unwrap();
        }
        std::fs::write(
            &path,
            format!("<table =\"{attributes}><tr><td>value</td></tr></table>"),
        )
        .unwrap();

        let error = read_html_tables(&path, &FormatSettings::default(), &Cancel::new())
            .err()
            .expect("the attribute budget rejects the tag");

        assert!(error.contains("attribute limit"));

        std::fs::write(&end_tag_path, format!("<table></table{attributes}>")).unwrap();
        let error = read_html_tables(&end_tag_path, &FormatSettings::default(), &Cancel::new())
            .err()
            .expect("the attribute budget also checks end tags");
        assert!(error.contains("attribute limit"));
    }

    #[test]
    fn html_wide_unspanned_rows_do_not_retain_span_storage() {
        let document = Html::parse_document(
            "<table><tr><td colspan=\"4000\">wide</td></tr><tr><td>narrow</td></tr></table>",
        );
        let table_selector = Selector::parse("table").unwrap();
        let row_selector = Selector::parse("tr").unwrap();
        let table = document.select(&table_selector).next().unwrap();
        let mut total_bytes = 0;
        let mut total_cells = 0;
        let cancel = Cancel::new();
        let mut builder = HtmlTableText::new(
            Path::new("wide.html"),
            "Wide",
            &mut total_bytes,
            &mut total_cells,
            &cancel,
        );

        for row in table.select(&row_selector) {
            builder.append_row(row, false).unwrap();
            assert!(builder.row_spans.is_empty());
        }
    }

    #[test]
    fn html_document_cell_limit_rejects_growth_before_allocating_the_row() {
        let document = Html::parse_document("<table><tr><td>value</td></tr></table>");
        let table_selector = Selector::parse("table").unwrap();
        let row_selector = Selector::parse("tr").unwrap();
        let table = document.select(&table_selector).next().unwrap();
        let row = table.select(&row_selector).next().unwrap();
        let mut total_bytes = 0;
        let mut total_cells = MAX_HTML_DOCUMENT_CELLS;
        let cancel = Cancel::new();
        let mut builder = HtmlTableText::new(
            Path::new("full.html"),
            "Full",
            &mut total_bytes,
            &mut total_cells,
            &cancel,
        );

        let error = builder.append_row(row, false).unwrap_err();

        assert!(error.contains("expanded HTML cell limit"));
    }

    #[test]
    fn html_file_without_tables_returns_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.html");
        let right = dir.path().join("right.csv");
        std::fs::write(&left, "<html><body>There is no table here.</body></html>").unwrap();
        std::fs::write(&right, "id,value\n1,ok\n").unwrap();
        let mut job = spawn_load(left, right, TableSettings::default(), Arc::new(|| {}));

        let messages = drain(&mut job);

        assert!(messages.into_iter().any(|message| matches!(
            message,
            TableMessage::Failed(reason)
                if reason.contains("contains no HTML tables that Table Compare can read")
        )));
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
