//! The persisted description of a class of files.
//!
//! A format answers four questions about the files its mask claims: which
//! comparison view handles them, what conversion runs before comparing and
//! before saving, what syntax they use, and what editor settings apply. A
//! [`FormatRegistry`] holds the formats in priority order and resolves a
//! filename to exactly one of them.

use crate::compat::{is_empty_map, Extensible, UnknownFields};
use crate::grammar::Grammar;
use crate::highlight::StyleMap;
use crate::mask::MaskList;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;

/// Which comparison view a format's files are opened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum FormatKind {
    /// Compared as lines of text.
    #[default]
    Text,
    /// Compared as rows and columns.
    Table,
    /// Compared byte by byte.
    Hex,
    /// Compared as images.
    Picture,
    /// Handed to an outside program.
    External,
}

/// How a file is turned into comparable text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ConversionMethod {
    /// The file is compared as it sits on disk.
    #[default]
    None,
    /// An outside program produces the text that is compared.
    ExternalProgram,
}

/// How filenames are handed to a conversion program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum FilenameEncoding {
    /// Filenames are passed as Unicode, which extended characters need.
    #[default]
    Unicode,
    /// Filenames are passed in the system's narrow encoding.
    Ansi,
}

/// The encoding a format assumes for its files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TextEncodingDefault {
    /// Detected from the whole file rather than a leading window, falling back
    /// to UTF-8 when detection is inconclusive.
    #[default]
    Detect,
    /// A specific code page, named the way the encoding tables name it.
    CodePage(String),
}

/// How lines end in files a format writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum LineEndingStyle {
    /// Keep whatever the file already uses.
    #[default]
    Preserve,
    /// Carriage return followed by line feed.
    CrLf,
    /// Line feed alone.
    Lf,
    /// Carriage return alone.
    Cr,
}

/// The three paths a conversion command line can be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConversionPaths<'a> {
    /// The file the conversion reads.
    pub source: &'a str,
    /// The file the conversion writes.
    pub target: &'a str,
    /// The file the text originally came from, which differs from the source
    /// once an earlier step has already produced a temporary file.
    pub original: &'a str,
}

/// A conversion command line, already split into the program and its
/// arguments.
///
/// The split happens once, on the template, so no part of a substituted path
/// can become an argument boundary, an extra argument, or a control operator.
/// Nothing here is quoted or escaped for a shell, because it is not meant to
/// reach one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionCommand {
    /// The program to run.
    pub program: OsString,
    /// The arguments, one entry per argument, in order.
    pub arguments: Vec<OsString>,
}

/// Settings on a format's conversion group.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionSettings {
    /// Whether an outside program is involved at all.
    #[serde(default)]
    pub method: Extensible<ConversionMethod>,
    /// Command line run when a file is read.
    #[serde(default)]
    pub loading_command: String,
    /// Command line run before a file is written. Only meaningful when editing
    /// is allowed.
    #[serde(default)]
    pub saving_command: String,
    /// Editing is refused for files of this format.
    #[serde(default)]
    pub disable_editing: bool,
    /// How filenames reach the conversion program.
    #[serde(default)]
    pub filename_encoding: Extensible<FilenameEncoding>,
    /// The encoding assumed for the converted text.
    #[serde(default)]
    pub encoding: Extensible<TextEncodingDefault>,
    /// Hex 1A is honored as an end-of-file marker and anything past it ignored.
    #[serde(default)]
    pub ctrl_z_ends_file: bool,
    /// Break display lines longer than this many characters. The breaks are
    /// undone on save, so they never reach the file.
    #[serde(default)]
    pub characters_per_line_limit: Option<u32>,
    /// Strip end-of-line whitespace when writing.
    #[serde(default)]
    pub trim_trailing_whitespace_on_save: bool,
    /// Turn leading spaces into tabs when writing, using the format's tab stop.
    #[serde(default)]
    pub leading_spaces_to_tabs_on_save: bool,
    /// Line ending written on save.
    #[serde(default)]
    pub line_ending: Extensible<LineEndingStyle>,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl Default for ConversionSettings {
    fn default() -> Self {
        Self {
            method: ConversionMethod::None.into(),
            loading_command: String::new(),
            saving_command: String::new(),
            disable_editing: false,
            filename_encoding: FilenameEncoding::Unicode.into(),
            encoding: Extensible::Known(TextEncodingDefault::Detect),
            ctrl_z_ends_file: false,
            characters_per_line_limit: None,
            trim_trailing_whitespace_on_save: false,
            leading_spaces_to_tabs_on_save: false,
            line_ending: LineEndingStyle::Preserve.into(),
            unknown: UnknownFields::new(),
        }
    }
}

impl ConversionSettings {
    /// Split a conversion command line into a program and its arguments,
    /// substituting the path variables.
    ///
    /// The template is split on whitespace outside double quotes, and a pair of
    /// double quotes groups one argument without becoming part of it. `%s` then
    /// becomes the source path, `%t` the target path and `%o` the original
    /// path, each dropped whole into the argument slot it appears in. `%%`
    /// yields a single percent sign, which is the only way to write one that is
    /// not read as the start of a variable.
    ///
    /// Returns `None` when the template names no program.
    ///
    /// # Invariants
    ///
    /// The result is for handing straight to a process spawning interface that
    /// takes a program and an argument vector. It must never be joined back
    /// into one string and must never be passed to a shell: argument boundaries
    /// are decided from the template alone, before any path is substituted, and
    /// re-parsing the substituted text would undo exactly that protection.
    ///
    /// # Limitations
    ///
    /// A path beginning with a dash arrives as one whole argument, so it cannot
    /// turn into several, but a program reading its arguments positionally may
    /// still take it for an option. A template guards against that only by
    /// embedding the variable in a larger argument or by placing the program's
    /// own end-of-options marker ahead of it.
    pub fn build_command(template: &str, paths: ConversionPaths<'_>) -> Option<ConversionCommand> {
        let mut tokens: Vec<String> = Vec::new();
        let mut current: Option<String> = None;
        let mut quoted = false;
        let mut chars = template.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    quoted = !quoted;
                    current.get_or_insert_with(String::new);
                }
                ' ' | '\t' if !quoted => {
                    if let Some(done) = current.take() {
                        tokens.push(done);
                    }
                }
                '%' => {
                    let slot = current.get_or_insert_with(String::new);
                    match chars.next() {
                        Some('s') => slot.push_str(paths.source),
                        Some('t') => slot.push_str(paths.target),
                        Some('o') => slot.push_str(paths.original),
                        Some('%') | None => slot.push('%'),
                        Some(other) => {
                            slot.push('%');
                            slot.push(other);
                        }
                    }
                }
                other => current.get_or_insert_with(String::new).push(other),
            }
        }
        if let Some(done) = current.take() {
            tokens.push(done);
        }
        let mut tokens = tokens.into_iter();
        Some(ConversionCommand {
            program: OsString::from(tokens.next()?),
            arguments: tokens.map(OsString::from).collect(),
        })
    }

    /// Whether a conversion run counts as successful.
    ///
    /// Both conditions are required: a program can exit cleanly having written
    /// nothing, and an empty result would otherwise be compared as if the file
    /// were genuinely empty.
    pub fn conversion_succeeded(exit_code: i32, output_len: u64) -> bool {
        exit_code == 0 && output_len > 0
    }
}

/// Settings on a text format's miscellaneous group.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiscSettings {
    /// The Tab key inserts spaces up to the next tab stop instead of a tab.
    #[serde(default)]
    pub insert_spaces_instead_of_tabs: bool,
    /// The interval between tab stops, which also drives the tab and space
    /// conversions.
    #[serde(default = "default_tab_stop")]
    pub tab_stop: u32,
    /// Each line is a standalone record, so consecutive differing lines are not
    /// grouped and navigation moves one line at a time.
    #[serde(default)]
    pub lines_are_independent: bool,
    /// Character position on a line carries meaning, so lines are compared
    /// column by column.
    #[serde(default)]
    pub column_based_data: bool,
    /// Weight added to every line of this format during alignment, on top of
    /// the per-element weights the grammar carries.
    #[serde(default)]
    pub base_line_weight: i32,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

fn default_tab_stop() -> u32 {
    8
}

impl Default for MiscSettings {
    fn default() -> Self {
        Self {
            insert_spaces_instead_of_tabs: false,
            tab_stop: default_tab_stop(),
            lines_are_independent: false,
            column_based_data: false,
            base_line_weight: 0,
            unknown: UnknownFields::new(),
        }
    }
}

/// How a table format's fields are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TableLayout {
    /// Decide between delimited and fixed position automatically.
    #[default]
    Detect,
    /// Fields are separated by a delimiter character.
    Delimited,
    /// Fields are defined by position on the line.
    Fixed,
}

/// What the first line of a table file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum FirstLineContents {
    /// Decide automatically.
    #[default]
    Detect,
    /// Column names.
    ColumnNames,
    /// Ordinary cell data.
    Data,
}

/// The order the parts of a date appear in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DateOrder {
    /// Month, day, year.
    #[default]
    Mdy,
    /// Day, month, year.
    Dmy,
    /// Year, month, day.
    Ymd,
}

/// Settings on a table format's regional group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionalSettings {
    /// Take every convention below from the operating system instead.
    #[serde(default = "crate::grammar::default_true")]
    pub use_system: bool,
    /// Character separating the whole and fractional parts of a number.
    #[serde(default)]
    pub decimal_separator: Option<char>,
    /// Character grouping the digits of a number.
    #[serde(default)]
    pub thousands_separator: Option<char>,
    /// Order the parts of a date appear in.
    #[serde(default)]
    pub date_order: Extensible<DateOrder>,
    /// Character between the parts of a date.
    #[serde(default)]
    pub date_separator: Option<char>,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl Default for RegionalSettings {
    fn default() -> Self {
        Self {
            use_system: true,
            decimal_separator: None,
            thousands_separator: None,
            date_order: DateOrder::Mdy.into(),
            date_separator: None,
            unknown: UnknownFields::new(),
        }
    }
}

/// Settings on a table format's type and regional groups.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSettings {
    /// How fields are laid out.
    #[serde(default)]
    pub layout: Extensible<TableLayout>,
    /// Characters that separate fields.
    #[serde(default)]
    pub delimiters: String,
    /// Character that may surround a field, needed when data holds a delimiter.
    #[serde(default)]
    pub text_qualifier: Option<char>,
    /// A run of delimiter characters counts as one delimiter.
    #[serde(default)]
    pub consecutive_delimiters_as_one: bool,
    /// Whitespace next to a delimiter belongs to the delimiter, not the field.
    #[serde(default = "crate::grammar::default_true")]
    pub whitespace_is_part_of_delimiter: bool,
    /// Field widths, for fixed position files.
    #[serde(default)]
    pub column_widths: Vec<u32>,
    /// What the first line holds.
    #[serde(default)]
    pub first_line: Extensible<FirstLineContents>,
    /// Numeric and date conventions.
    #[serde(default)]
    pub regional: RegionalSettings,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

/// Command lines an external format hands its files to.
///
/// The quick forms report their verdict through the exit code: zero means the
/// files match, one means they differ.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSettings {
    /// Run when a verdict is needed without a view.
    #[serde(default)]
    pub quick_compare_command: String,
    /// Run when a comparison view is needed.
    #[serde(default)]
    pub compare_view_command: String,
    /// Run when a merge verdict is needed without a view.
    #[serde(default)]
    pub quick_merge_command: String,
    /// Run when a merge view is needed.
    #[serde(default)]
    pub merge_view_command: String,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl ExternalSettings {
    /// Read a quick command's exit code as a verdict.
    ///
    /// Returns `None` for any code other than the two documented ones, which
    /// means the tool failed rather than reached a verdict.
    pub fn verdict_of(exit_code: i32) -> Option<bool> {
        match exit_code {
            0 => Some(true),
            1 => Some(false),
            _ => None,
        }
    }
}

/// One file format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFormat {
    /// The name shown in the format list.
    pub name: String,
    /// Free text; stock formats use it to state limitations and requirements.
    #[serde(default)]
    pub description: String,
    /// The filenames this format claims. An empty list claims nothing, leaving
    /// the format reachable only by an explicit choice.
    #[serde(default)]
    pub masks: MaskList,
    /// Which comparison view handles the files.
    #[serde(default)]
    pub kind: Extensible<FormatKind>,
    /// Whether the format takes part in lookup at all.
    #[serde(default = "crate::grammar::default_true")]
    pub enabled: bool,
    /// Conversion and encoding group.
    #[serde(default)]
    pub conversion: ConversionSettings,
    /// Syntax definition, used for coloring and for the importance checklist.
    #[serde(default)]
    pub grammar: Grammar,
    /// Element to color role assignments.
    #[serde(default)]
    pub styles: StyleMap,
    /// Editor and comparison settings for text files.
    #[serde(default)]
    pub misc: MiscSettings,
    /// Field layout and regional settings, for a table format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<TableSettings>,
    /// Command lines, for an external format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalSettings>,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl FileFormat {
    /// A text format named `name` claiming `masks`.
    ///
    /// The mask list is stored even when it does not compile; a list that fails
    /// to compile claims nothing, which is the same outcome as an empty list.
    pub fn text(name: impl Into<String>, masks: &str) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            masks: MaskList::new(masks).unwrap_or_default(),
            kind: FormatKind::Text.into(),
            enabled: true,
            conversion: ConversionSettings::default(),
            grammar: Grammar::empty(),
            styles: StyleMap::new(),
            misc: MiscSettings::default(),
            table: None,
            external: None,
            unknown: UnknownFields::new(),
        }
    }

    /// The same format with a description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The same format with a grammar.
    #[must_use]
    pub fn with_grammar(mut self, grammar: Grammar) -> Self {
        self.grammar = grammar;
        self
    }

    /// The same format with a tab stop.
    #[must_use]
    pub fn with_tab_stop(mut self, tab_stop: u32) -> Self {
        self.misc.tab_stop = tab_stop;
        self
    }

    /// The same format under a different comparison view.
    #[must_use]
    pub fn with_kind(mut self, kind: FormatKind) -> Self {
        self.kind = kind.into();
        self
    }

    /// Whether this format claims `path`, taking the enabled flag into account.
    pub fn claims(&self, path: &str) -> bool {
        self.enabled && self.masks.matches(path)
    }
}

/// Formats in priority order, plus the format used when nothing matches.
///
/// Lookup walks the list from the top and stops at the first enabled entry
/// whose mask matches, so moving an entry up or down is what resolves an
/// overlap between two masks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatRegistry {
    /// The formats, highest priority first.
    #[serde(default)]
    pub formats: Vec<FileFormat>,
    /// The format for a filename no entry claims.
    pub fallback: FileFormat,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl FormatRegistry {
    /// A registry holding only a fallback.
    pub fn new(fallback: FileFormat) -> Self {
        Self {
            formats: Vec::new(),
            fallback,
            unknown: UnknownFields::new(),
        }
    }

    /// Append a format at the lowest priority.
    pub fn push(&mut self, format: FileFormat) {
        self.formats.push(format);
    }

    /// The format that handles `path`.
    pub fn lookup(&self, path: &str) -> &FileFormat {
        self.formats
            .iter()
            .find(|f| f.claims(path))
            .unwrap_or(&self.fallback)
    }

    /// The index of the format that handles `path`, or `None` for the fallback.
    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.formats.iter().position(|f| f.claims(path))
    }

    /// The format named `name`, if the registry holds one.
    pub fn by_name(&self, name: &str) -> Option<&FileFormat> {
        self.formats.iter().find(|f| f.name == name)
    }

    /// Raise the format at `index` one position.
    ///
    /// Returns the new index, or `None` when the entry is already at the top or
    /// the index is out of range.
    pub fn move_up(&mut self, index: usize) -> Option<usize> {
        if index == 0 || index >= self.formats.len() {
            return None;
        }
        self.formats.swap(index - 1, index);
        Some(index - 1)
    }

    /// Lower the format at `index` one position.
    ///
    /// Returns the new index, or `None` when the entry is already at the bottom
    /// or the index is out of range.
    pub fn move_down(&mut self, index: usize) -> Option<usize> {
        if index + 1 >= self.formats.len() {
            return None;
        }
        self.formats.swap(index, index + 1);
        Some(index + 1)
    }

    /// Whether every filename the entry at `index` could claim is already
    /// claimed above it, which makes the entry unreachable.
    ///
    /// The test compares mask patterns rather than the languages they generate,
    /// so it recognizes an entry shadowed by an identical or catch-all pattern
    /// and misses one shadowed by a strictly broader but differently written
    /// pattern.
    pub fn is_shadowed(&self, index: usize) -> bool {
        let Some(entry) = self.formats.get(index) else {
            return false;
        };
        if !entry.enabled || entry.masks.is_empty() {
            return false;
        }
        let above: Vec<&FileFormat> = self.formats[..index].iter().filter(|f| f.enabled).collect();
        entry.patterns_are_covered(&above)
    }
}

impl FileFormat {
    /// Whether every pattern of this format appears, verbatim or as a catch-all,
    /// among `others`.
    fn patterns_are_covered(&self, others: &[&Self]) -> bool {
        self.masks.patterns().all(|pattern| {
            others.iter().any(|other| {
                other
                    .masks
                    .patterns()
                    .any(|p| p == "*" || p == "*.*" || p.eq_ignore_ascii_case(pattern))
            })
        })
    }
}
