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
    /// True when decoding needed replacement characters.
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
            compare_tables(left, right, &settings, emitter, cancel, None);
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
            compare_tables(left, right, &settings, emitter, cancel, None);
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
    let left_text = match read_input(left, &settings.left_format, clipboard[0].as_deref()) {
        Ok(text) => text,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    let right_text = match read_input(right, &settings.right_format, clipboard[1].as_deref()) {
        Ok(text) => text,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(TableMessage::Progress(Stage::Detecting));
    let left_options = resolve(
        &left_text.text,
        &settings.left_format,
        settings,
        &settings.schema.left_regional,
    );
    let right_options = resolve(
        &right_text.text,
        &settings.right_format,
        settings,
        &settings.schema.right_regional,
    );
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(TableMessage::Progress(Stage::Parsing));
    let texts = (
        Arc::new(left_text.text.clone()),
        Arc::new(right_text.text.clone()),
    );
    let left_side = match build_side(left_text, &left_options) {
        Ok(side) => side,
        Err(message) => {
            emitter.send(TableMessage::Failed(message));
            return;
        }
    };
    let right_side = match build_side(right_text, &right_options) {
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
        left_side,
        right_side,
        settings,
        emitter,
        cancel,
        Some(texts),
    );
}

fn compare_tables(
    left: Side,
    right: Side,
    settings: &TableSettings,
    emitter: &Emitter<TableMessage>,
    cancel: &Cancel,
    texts: Option<(Arc<String>, Arc<String>)>,
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

fn build_side(text: SideText, options: &ParseOptions) -> Result<Side, String> {
    let table = parse(&text.text, options).map_err(|error| error.to_string())?;
    let mut resolved_options = options.clone();
    resolved_options.first_line_contains = table.first_line_contains().clone();
    let facts = SideFacts {
        encoding: text.encoding,
        had_errors: text.had_errors,
        syntax: syntax_label(options),
        header: header_label(&table),
        warnings: table.warnings().to_vec(),
        options: resolved_options,
        stamp: text.stamp,
        template: Some(text.template),
    };
    Ok(Side {
        table: Arc::new(table),
        facts,
    })
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
        read_side_with_open, spawn_load, spawn_recompare, FormatSettings, Pipeline, Side,
        TableMessage, TableSettings,
    };
    use ca_table::compare::CellStatus;
    use ca_table::schema::{ColumnAlignment, ColumnHandling, ColumnType, ManualColumnPair};
    use std::fmt::Write as _;
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
