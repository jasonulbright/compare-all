//! The background work behind a text comparison: reading both files, aligning
//! them, classifying the differences and locating the changed characters.
//!
//! The whole pipeline runs on one worker thread and posts a single finished
//! result, so nothing but painting ever happens on the frame thread.

use crate::lines::{self, LineMetrics};
use crate::model::{self, RowModel};
use ca_diff::importance::ReplacementRule;
use ca_diff::lines::{diff_line_slices_anchored_cancellable, AlignAnchor};
use ca_diff::{
    classify_hunks_cancellable, classify_hunks_indexed_cancellable, diff_chars_cancellable,
    ClassifiedHunk, HunkKind, InlineOptions, LineCompareOptions, RuleSet, WhitespaceClassifier,
};
use ca_grammar::{Grammar, IndexedGrammarClassifier, StyleMap, StyleSlot};
use ca_text::{DecodeOptions, EolStyle, LoadedText};
use ca_ui::save::{FileSystem, RealFileSystem, Stamp};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Upper bound on the rows that get character level highlighting.
///
/// Locating changed characters costs time and memory proportional to the text
/// of every changed row, so a comparison with more changed rows than this keeps
/// its line level coloring and drops the inline spans.
const INLINE_ROW_LIMIT: usize = 100_000;

/// Which classes of grammar element hold differences that count.
///
/// A format names its elements freely, so each element of the active format is
/// assigned to the class its style slot names and takes that class's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ElementRules {
    /// Differences inside commentary count.
    pub comments: bool,
    /// Differences inside string and character literals count.
    pub strings: bool,
    /// Differences inside numeric literals count.
    pub numbers: bool,
    /// Differences inside reserved words count.
    pub keywords: bool,
    /// Differences inside user-chosen names count.
    pub identifiers: bool,
}

impl Default for ElementRules {
    /// Every class counts, which is the stored default of an element the
    /// importance list does not name.
    fn default() -> Self {
        Self {
            comments: true,
            strings: true,
            numbers: true,
            keywords: true,
            identifiers: true,
        }
    }
}

impl ElementRules {
    /// True when differences in the class `slot` names count.
    ///
    /// A slot no class covers counts, so an element the checklist has no entry
    /// for keeps the behavior of a comparison with no rules at all.
    #[must_use]
    pub const fn slot_important(self, slot: StyleSlot) -> bool {
        match slot {
            StyleSlot::Comment => self.comments,
            StyleSlot::Literal => self.strings,
            StyleSlot::Number => self.numbers,
            StyleSlot::Keyword => self.keywords,
            StyleSlot::Identifier => self.identifiers,
            _ => true,
        }
    }
}

/// The importance rules a comparison runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct RuleToggles {
    /// Which classes of grammar element count.
    pub elements: ElementRules,
    /// Whitespace before the first non-whitespace character counts.
    pub leading_whitespace_important: bool,
    /// Whitespace between non-whitespace runs counts.
    pub embedded_whitespace_important: bool,
    /// Whitespace after the last non-whitespace character counts.
    pub trailing_whitespace_important: bool,
    /// Text no grammar element claims counts.
    pub everything_else_important: bool,
    /// Differences in letter case are unimportant.
    pub case_unimportant: bool,
    /// A line present on one side only counts whatever it holds.
    pub orphan_lines_always_important: bool,
}

impl Default for RuleToggles {
    /// The starting rules, which are the stored defaults of the importance
    /// group. A test holds the two in step.
    fn default() -> Self {
        Self {
            elements: ElementRules::default(),
            leading_whitespace_important: true,
            embedded_whitespace_important: true,
            trailing_whitespace_important: true,
            everything_else_important: true,
            case_unimportant: true,
            orphan_lines_always_important: true,
        }
    }
}

impl RuleToggles {
    /// True when no whitespace position counts.
    #[must_use]
    pub const fn whitespace_unimportant(self) -> bool {
        !self.leading_whitespace_important
            && !self.embedded_whitespace_important
            && !self.trailing_whitespace_important
    }

    /// Sets every whitespace position at once, which is what the toolbar's
    /// single whitespace toggle edits.
    pub fn set_whitespace_unimportant(&mut self, unimportant: bool) {
        self.leading_whitespace_important = !unimportant;
        self.embedded_whitespace_important = !unimportant;
        self.trailing_whitespace_important = !unimportant;
    }
}

/// Everything a comparison needs beyond the two texts.
#[derive(Debug, Clone, Default)]
pub struct CompareSettings {
    /// The syntax definition of the left file's format.
    pub grammar: Grammar,
    /// Which color role each element name of that format plays, which is how an
    /// element is assigned to an importance class.
    pub styles: StyleMap,
    /// The rules in force.
    pub rules: RuleToggles,
    /// What the line pass disregards, and how lines are paired.
    pub compare: LineCompareOptions,
    /// Substitutions that make two spellings equivalent.
    pub replacements: Vec<ReplacementRule>,
    /// Pairings the user forced, in increasing line order.
    pub anchors: Vec<AlignAnchor>,
    /// How the left file's bytes are decoded.
    pub left_decode: DecodeOptions,
    /// How the right file's bytes are decoded.
    pub right_decode: DecodeOptions,
}

/// What one side of the comparison holds once it is loaded.
#[derive(Debug, Clone, Default)]
pub struct SidePayload {
    /// The file's lines, without their endings.
    pub lines: Arc<Vec<String>>,
    /// Display width of each line, in the same order.
    pub metrics: Vec<LineMetrics>,
    /// Label of the encoding the bytes were decoded with.
    pub encoding: String,
    /// Label of the dominant line ending style.
    pub eol: String,
    /// True when the file holds more than one line ending style.
    pub mixed_eol: bool,
    /// True when decoding needed replacement characters.
    pub had_errors: bool,
}

impl SidePayload {
    /// The metrics of one line, or a zero width line where there is none.
    #[must_use]
    pub fn metrics(&self, line: u32) -> LineMetrics {
        self.metrics.get(line as usize).copied().unwrap_or_default()
    }
}

/// A run of changed characters, as columns rather than bytes.
///
/// Byte offsets are what the engine produces and columns are what a monospaced
/// pane paints, so the conversion happens once here rather than per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSpan {
    /// First column of the run.
    pub start: u32,
    /// One past the last column of the run.
    pub end: u32,
}

/// The changed characters of one row, on both sides.
#[derive(Debug, Clone, Default)]
pub struct InlineColumns {
    /// Left side runs.
    pub left: Vec<ColumnSpan>,
    /// Right side runs.
    pub right: Vec<ColumnSpan>,
}

/// A finished comparison.
#[derive(Debug, Clone, Default)]
pub struct TextData {
    /// Left side content and facts.
    pub left: SidePayload,
    /// Right side content and facts.
    pub right: SidePayload,
    /// Row layout of the two sides.
    pub model: RowModel,
    /// Changed character runs as columns, keyed by row index.
    pub inline: HashMap<u32, InlineColumns>,
    /// True when the comparison was too large for inline spans.
    pub inline_omitted: bool,
    /// Widest line on either side, in columns, which is how far the panes
    /// scroll sideways.
    pub widest: u32,
    /// What the files held when they were read, which is what a save writes
    /// back through. Only the loading path fills these in; a comparison that
    /// followed an edit leaves them empty so the view keeps the ones it has.
    pub sources: Option<Box<Sources>>,
}

/// The loaded files behind a comparison.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The left file as it was read.
    pub left: LoadedText,
    /// The right file as it was read.
    pub right: LoadedText,
    /// What the left file looked like on disk when it was read.
    pub left_stamp: Option<Stamp>,
    /// What the right file looked like on disk when it was read.
    pub right_stamp: Option<Stamp>,
}

/// What the worker posts back.
#[derive(Debug)]
pub enum TextMessage {
    /// A step of the pipeline has begun.
    Progress(&'static str),
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
    /// The comparison is ready.
    Ready(Box<TextData>),
}

impl Terminal for TextMessage {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            TextMessage::Failed(_) | TextMessage::Cancelled | TextMessage::Ready(_)
        )
    }

    fn cancelled() -> Self {
        TextMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        TextMessage::Failed(detail)
    }
}

/// Load and compare two files on a worker thread.
pub fn spawn(
    left: PathBuf,
    right: PathBuf,
    settings: CompareSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<TextMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| run(&left, &right, &settings, emitter, cancel),
        notify,
    )
}

fn run(
    left: &Path,
    right: &Path,
    settings: &CompareSettings,
    emitter: &Emitter<TextMessage>,
    cancel: &Cancel,
) {
    emitter.send(TextMessage::Progress("Reading files"));
    let left_loaded = match load(left, &settings.left_decode) {
        Ok(loaded) => loaded,
        Err(message) => {
            emitter.send(TextMessage::Failed(message));
            return;
        }
    };
    let right_loaded = match load(right, &settings.right_decode) {
        Ok(loaded) => loaded,
        Err(message) => {
            emitter.send(TextMessage::Failed(message));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    let (left_text, left_payload, left_source, left_stamp) = left_loaded;
    let (right_text, right_payload, right_source, right_stamp) = right_loaded;
    if (left_source.had_errors || right_source.had_errors)
        && !left_source.original_bytes_equal(&right_source)
    {
        emitter.send(TextMessage::Failed(
            "the files contain different bytes that could not be decoded; choose an encoding before comparing them"
                .to_owned(),
        ));
        return;
    }
    let sources = Some(Box::new(Sources {
        left: left_source,
        right: right_source,
        left_stamp,
        right_stamp,
    }));
    compare(
        &left_text,
        &right_text,
        left_payload,
        right_payload,
        sources,
        settings,
        emitter,
        cancel,
    );
}

/// Compare two texts already in memory and post the result.
///
/// This is the path an edit takes: nothing is read from disk, so a re-diff
/// never observes a file that changed under the buffer.
pub fn spawn_texts(
    left_text: String,
    right_text: String,
    left_payload: SidePayload,
    right_payload: SidePayload,
    settings: CompareSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<TextMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            compare(
                &left_text,
                &right_text,
                left_payload,
                right_payload,
                None,
                &settings,
                emitter,
                cancel,
            );
        },
        notify,
    )
}

#[allow(clippy::too_many_arguments)]
fn compare(
    left_text: &str,
    right_text: &str,
    left_payload: SidePayload,
    right_payload: SidePayload,
    sources: Option<Box<Sources>>,
    settings: &CompareSettings,
    emitter: &Emitter<TextMessage>,
    cancel: &Cancel,
) {
    emitter.send(TextMessage::Progress("Comparing"));
    let left_lines = ca_diff::split_lines(left_text);
    let right_lines = ca_diff::split_lines(right_text);
    let options = settings.compare.clone();
    // The engine refuses the whole comparison over an anchor past either
    // side's end; a stored alignment can outlive the lines it named.
    let anchors: Vec<AlignAnchor> = settings
        .anchors
        .iter()
        .filter(|anchor| {
            usize::try_from(anchor.left.end).is_ok_and(|end| end <= left_lines.len())
                && usize::try_from(anchor.right.end).is_ok_and(|end| end <= right_lines.len())
        })
        .cloned()
        .collect();
    let hunks = match diff_line_slices_anchored_cancellable(
        &left_lines,
        &right_lines,
        &options,
        &anchors,
        cancel,
    ) {
        Ok(hunks) => hunks,
        Err(ca_diff::DiffError::Cancelled) => return,
        Err(error) => {
            emitter.send(TextMessage::Failed(error.to_string()));
            return;
        }
    };
    let classified = match classify(&left_lines, &right_lines, &hunks, settings, cancel) {
        Ok(classified) => classified,
        Err(ClassificationError::Cancelled) => return,
        Err(ClassificationError::Failed(error)) => {
            emitter.send(TextMessage::Failed(error));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    let model = model::build(&classified);
    emitter.send(TextMessage::Progress("Highlighting"));
    let (inline, inline_omitted) =
        inline_spans(&classified, &left_lines, &right_lines, &model, cancel);
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(TextMessage::Progress("Measuring"));
    let left_metrics: Vec<LineMetrics> =
        left_lines.iter().map(|line| lines::measure(line)).collect();
    let right_metrics: Vec<LineMetrics> = right_lines
        .iter()
        .map(|line| lines::measure(line))
        .collect();
    if cancel.is_cancelled() {
        return;
    }
    let widest = left_metrics
        .iter()
        .chain(right_metrics.iter())
        .map(|metrics| metrics.columns)
        .max()
        .unwrap_or(0);

    let data = TextData {
        left: SidePayload {
            lines: Arc::new(left_lines.iter().map(|line| (*line).to_string()).collect()),
            metrics: left_metrics,
            ..left_payload
        },
        right: SidePayload {
            lines: Arc::new(right_lines.iter().map(|line| (*line).to_string()).collect()),
            metrics: right_metrics,
            ..right_payload
        },
        model,
        inline,
        inline_omitted,
        widest,
        sources,
    };
    emitter.send(TextMessage::Ready(Box::new(data)));
}

/// Split the hunks into important and unimportant.
///
/// A format with a grammar classifies through the grammar, so an element the
/// rules exclude stops being an important difference. A format without one
/// falls back to the whitespace split, which is all the engine can tell.
#[derive(Debug)]
enum ClassificationError {
    Cancelled,
    Failed(String),
}

impl From<ca_diff::DiffError> for ClassificationError {
    fn from(error: ca_diff::DiffError) -> Self {
        if error == ca_diff::DiffError::Cancelled {
            Self::Cancelled
        } else {
            Self::Failed(error.to_string())
        }
    }
}

fn classify(
    left: &[&str],
    right: &[&str],
    hunks: &[ca_diff::Hunk],
    settings: &CompareSettings,
    cancel: &Cancel,
) -> Result<Vec<ClassifiedHunk>, ClassificationError> {
    if settings.grammar.items.is_empty() {
        let mut rules = RuleSet::all_important();
        apply_toggles(&mut rules, settings);
        return classify_hunks_cancellable(
            left,
            right,
            hunks,
            &rules,
            &WhitespaceClassifier,
            cancel,
        )
        .map_err(ClassificationError::from);
    }
    let classifier = IndexedGrammarClassifier::new(&settings.grammar, left, right)
        .map_err(|error| ClassificationError::Failed(error.to_string()))?;
    let mut rules = RuleSet::all_important();
    rules
        .case_sensitive_elements
        .extend(classifier.case_sensitive_elements());
    // The element list is a whitelist, so every element the grammar names is
    // important until its class takes it out.
    for name in classifier.element_names() {
        if settings
            .rules
            .elements
            .slot_important(settings.styles.slot_for(name))
        {
            rules.important_elements.insert(name.clone());
        }
    }
    apply_toggles(&mut rules, settings);
    classify_hunks_indexed_cancellable(left, right, hunks, &rules, &classifier, cancel)
        .map_err(ClassificationError::from)
}

fn apply_toggles(rules: &mut RuleSet, settings: &CompareSettings) {
    let toggles = settings.rules;
    rules.replacements.clone_from(&settings.replacements);
    rules.compare_line_endings = !settings.compare.ignore_line_endings;
    rules.match_character_case = !toggles.case_unimportant;
    rules.leading_whitespace_important = toggles.leading_whitespace_important;
    rules.embedded_whitespace_important = toggles.embedded_whitespace_important;
    rules.trailing_whitespace_important = toggles.trailing_whitespace_important;
    rules.everything_else_important = toggles.everything_else_important;
    rules.orphan_lines_always_important = toggles.orphan_lines_always_important;
}

type Loaded = (String, SidePayload, LoadedText, Option<Stamp>);

fn load(path: &Path, decode: &DecodeOptions) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let stamp = RealFileSystem.stamp(path);
    let loaded = LoadedText::load(&bytes, decode);
    let text = loaded.buffer.text();
    let payload = SidePayload {
        lines: Arc::new(Vec::new()),
        metrics: Vec::new(),
        encoding: loaded.spec.encoding.label().to_string(),
        eol: eol_label(loaded.eol.dominant).to_string(),
        mixed_eol: loaded.eol.mixed,
        had_errors: loaded.had_errors,
    };
    Ok((text, payload, loaded, stamp))
}

const fn eol_label(style: EolStyle) -> &'static str {
    match style {
        EolStyle::Lf => "Unix",
        EolStyle::CrLf => "Windows",
        EolStyle::Cr => "Mac",
    }
}

fn inline_spans(
    classified: &[ClassifiedHunk],
    left_lines: &[&str],
    right_lines: &[&str],
    model: &RowModel,
    cancel: &Cancel,
) -> (HashMap<u32, InlineColumns>, bool) {
    let changed: usize = classified
        .iter()
        .filter(|hunk| hunk.hunk.kind == HunkKind::Changed)
        .map(|hunk| hunk.hunk.left.len().min(hunk.hunk.right.len()))
        .sum();
    if changed > INLINE_ROW_LIMIT {
        return (HashMap::new(), true);
    }
    let mut spans = HashMap::with_capacity(changed);
    for (index, row) in model.rows().iter().enumerate() {
        if cancel.is_cancelled() {
            return (spans, false);
        }
        let (Some(left), Some(right)) = (row.left, row.right) else {
            continue;
        };
        if !row.class.is_difference() {
            continue;
        }
        let (Some(left_text), Some(right_text)) = (
            left_lines.get(left as usize),
            right_lines.get(right as usize),
        ) else {
            continue;
        };
        let Ok(diff) =
            diff_chars_cancellable(left_text, right_text, InlineOptions::default(), cancel)
        else {
            return (spans, false);
        };
        if !diff.is_empty() {
            #[allow(clippy::cast_possible_truncation)]
            spans.insert(
                index as u32,
                InlineColumns {
                    left: to_columns(left_text, &diff.left),
                    right: to_columns(right_text, &diff.right),
                },
            );
        }
    }
    (spans, false)
}

/// Byte runs to column runs, done once so a frame never counts characters.
fn to_columns(line: &str, spans: &[ca_diff::Span]) -> Vec<ColumnSpan> {
    spans
        .iter()
        .map(|span| ColumnSpan {
            start: lines::column_of_byte(line, span.start as usize),
            end: lines::column_of_byte(line, span.end as usize),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{spawn, CompareSettings, RuleToggles, TextMessage};
    use ca_diff::{diff_line_slices, Importance};
    use ca_grammar::builtin;
    use ca_text::{DecodeOptions, TextEncoding};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn collect(left: &std::path::Path, right: &std::path::Path) -> Vec<TextMessage> {
        collect_with_settings(left, right, CompareSettings::default())
    }

    fn collect_with_settings(
        left: &std::path::Path,
        right: &std::path::Path,
        settings: CompareSettings,
    ) -> Vec<TextMessage> {
        let mut job = spawn(
            left.to_path_buf(),
            right.to_path_buf(),
            settings,
            Arc::new(|| {}),
        );
        let deadline = Instant::now() + Duration::from_secs(20);
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

    #[test]
    fn different_bytes_that_decode_to_the_same_replacement_do_not_report_same() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, b"caf\xFF\n").unwrap();
        std::fs::write(&right, b"caf\xFE\n").unwrap();
        let forced_utf8 = DecodeOptions {
            forced: Some(TextEncoding::Utf8),
            ..DecodeOptions::default()
        };
        let messages = collect_with_settings(
            &left,
            &right,
            CompareSettings {
                left_decode: forced_utf8,
                right_decode: forced_utf8,
                ..CompareSettings::default()
            },
        );

        assert!(!messages
            .iter()
            .any(|message| matches!(message, TextMessage::Ready(_))));
        assert!(messages.iter().any(|message| matches!(
            message,
            TextMessage::Failed(reason) if reason.contains("different bytes")
        )));
    }

    #[test]
    fn a_stored_alignment_past_the_end_of_a_file_does_not_stop_the_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, b"a\nb\n").unwrap();
        std::fs::write(&right, b"a\nc\n").unwrap();
        let messages = collect_with_settings(
            &left,
            &right,
            CompareSettings {
                anchors: vec![
                    ca_diff::lines::AlignAnchor {
                        left: 0..1,
                        right: 0..1,
                    },
                    ca_diff::lines::AlignAnchor {
                        left: 3..4,
                        right: 3..4,
                    },
                ],
                ..CompareSettings::default()
            },
        );

        let ready = messages
            .into_iter()
            .find_map(|message| match message {
                TextMessage::Ready(data) => Some(data),
                _ => None,
            })
            .expect("the comparison finished");
        assert_eq!(ready.model.counts().differences, 1);
    }

    #[test]
    fn two_files_compare_to_a_row_model() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, b"alpha\nbeta\ngamma\n").unwrap();
        std::fs::write(&right, b"alpha\nbetta\ngamma\n").unwrap();
        let messages = collect(&left, &right);
        let ready = messages
            .into_iter()
            .find_map(|message| match message {
                TextMessage::Ready(data) => Some(data),
                _ => None,
            })
            .expect("the comparison finished");
        assert_eq!(ready.left.lines.len(), 3);
        assert_eq!(ready.model.counts().differences, 1);
        assert_eq!(ready.left.encoding, "UTF-8");
        assert_eq!(ready.right.eol, "Unix");
        assert!(!ready.inline.is_empty());
        assert!(!ready.inline_omitted);
    }

    /// The view's toggle and the session setting state the same default, which
    /// is a measurement of the behavior being mirrored.
    #[test]
    fn letter_case_starts_unimportant() {
        let toggles = RuleToggles::default();
        assert!(toggles.case_unimportant);
        assert_eq!(toggles.elements, super::ElementRules::default());
        assert!(!toggles.whitespace_unimportant());
        let settings = ca_session::settings::text::TextImportanceSettings::default();
        assert_eq!(toggles.case_unimportant, !settings.character_case_important);
    }

    #[test]
    fn a_case_only_pair_is_an_unimportant_difference_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, b"alpha\nbeta\n").unwrap();
        std::fs::write(&right, b"alpha\nBETA\n").unwrap();
        let messages = collect(&left, &right);
        let ready = messages
            .into_iter()
            .find_map(|message| match message {
                TextMessage::Ready(data) => Some(data),
                _ => None,
            })
            .expect("the comparison finished");
        let counts = ready.model.counts();
        assert_eq!(counts.differences, 1);
        assert_eq!(counts.unimportant, 1);
        assert_eq!(counts.important, 0);
    }

    #[test]
    fn case_sensitive_grammar_elements_override_the_global_case_toggle() {
        let left = ["const char *key = \"Secret\";\n"];
        let right = ["const char *key = \"secret\";\n"];
        let mut settings = super::CompareSettings {
            grammar: builtin::c_cpp().grammar,
            ..super::CompareSettings::default()
        };
        for item in &mut settings.grammar.items {
            if item.element == "String" {
                item.case_sensitive = true;
            }
        }
        settings.rules.case_unimportant = true;
        let hunks = diff_line_slices(&left, &right, &settings.compare);
        let cancel = super::Cancel::new();
        let classified = super::classify(&left, &right, &hunks, &settings, &cancel).unwrap();
        assert_eq!(classified[0].importance, Some(Importance::Important));
    }

    #[test]
    fn a_missing_file_reports_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("present.txt");
        std::fs::write(&left, b"x\n").unwrap();
        let messages = collect(&left, &dir.path().join("absent.txt"));
        assert!(messages
            .iter()
            .any(|message| matches!(message, TextMessage::Failed(_))));
    }

    #[test]
    fn line_ending_style_is_reported_per_side() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("crlf.txt");
        let right = dir.path().join("lf.txt");
        std::fs::write(&left, b"one\r\ntwo\r\n").unwrap();
        std::fs::write(&right, b"one\ntwo\n").unwrap();
        let messages = collect(&left, &right);
        let ready = messages
            .into_iter()
            .find_map(|message| match message {
                TextMessage::Ready(data) => Some(data),
                _ => None,
            })
            .expect("the comparison finished");
        assert_eq!(ready.left.eol, "Windows");
        assert_eq!(ready.right.eol, "Unix");
    }
}
