//! Writing the merge result, and what the caller learns from it.
//!
//! Three rules shape this module, because each one is a way a user loses work:
//!
//! - a section that still needs review is written with markers, while a
//!   suppressed conflict keeps its ancestor text;
//! - the encode can fail, and a text the output encoding cannot represent is
//!   reported rather than written;
//! - the target is checked against what it looked like when the session started
//!   before anything is written.
//!
//! The first two are this module's own. The third, the temporary file and the
//! rename over the target are the shared save.

use crate::jobs::Sources;
use crate::model::MergeModel;
use ca_text::{EditSnapshot, LineRange, LoadedText, TextBuffer};
use ca_ui::save::text::SaveConsent;
use ca_ui::save::{Baseline, FileSystem, SaveOutcome};
use std::path::Path;

/// Exit code for a merge that finished with nothing left to review.
pub const EXIT_SUCCESS: i32 = 0;
/// Exit code for a merge whose output was written with conflicts still in it.
pub const EXIT_CONFLICTS: i32 = 14;
/// Exit code for a merge that ended with conflicts and wrote no output.
pub const EXIT_CONFLICTS_NOT_WRITTEN: i32 = 101;
/// Exit code for a merge that failed for a reason of its own.
pub const EXIT_ERROR: i32 = 100;

/// The names the conflict markers carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerLabels {
    /// Name written on the opening marker.
    pub left: String,
    /// Name written on the ancestor marker.
    pub center: String,
    /// Name written on the closing marker.
    pub right: String,
}

/// Rope snapshots of the inputs that supply marker blocks.
#[derive(Debug)]
pub(crate) struct MarkerSaveInputs {
    pub(crate) left: EditSnapshot,
    pub(crate) center: Option<EditSnapshot>,
    pub(crate) right: EditSnapshot,
}

impl MarkerSaveInputs {
    /// Share the input text without copying it on the UI thread.
    pub(crate) fn from_sources(sources: &Sources) -> Self {
        Self {
            left: sources.left.buffer.edit_snapshot(),
            center: sources
                .center
                .as_ref()
                .map(|source| source.buffer.edit_snapshot()),
            right: sources.right.buffer.edit_snapshot(),
        }
    }
}

/// The small piece of merge state a marker save needs from the UI thread.
/// The input and output text remain in their rope-backed buffers and are read
/// by the save worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkerSavePlan {
    two_way: bool,
    sections: Vec<MarkerSaveSection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkerSaveSection {
    index: usize,
    output: std::ops::Range<u32>,
    left: std::ops::Range<u32>,
    center: std::ops::Range<u32>,
    right: std::ops::Range<u32>,
}

impl MarkerSavePlan {
    /// Copy conflict ranges without cloning the merge model or its text.
    pub(crate) fn from_model(model: &MergeModel) -> Self {
        let mut sections = Vec::new();
        for (index, section) in model.sections().iter().enumerate() {
            if !section.is_unresolved_conflict() {
                continue;
            }
            if let Some(output) = model.output_range(index) {
                sections.push(MarkerSaveSection {
                    index,
                    output,
                    left: section.left.clone(),
                    center: section.center.clone(),
                    right: section.right.clone(),
                });
            }
        }
        Self {
            two_way: model.inputs().two_way,
            sections,
        }
    }

    /// Sections whose markers the save will write.
    pub(crate) fn waiting_sections(&self) -> Vec<usize> {
        self.sections.iter().map(|section| section.index).collect()
    }
}

impl Default for MarkerLabels {
    fn default() -> Self {
        Self {
            left: "left".to_owned(),
            center: "center".to_owned(),
            right: "right".to_owned(),
        }
    }
}

/// How a merge ended, for the caller that started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Sections still waiting for review.
    pub conflicts_remaining: u32,
    /// True when the output file was written.
    pub written: bool,
    /// True when the session ended on an error of its own.
    pub failed: bool,
}

/// The exit code a merge reports to whatever started it.
#[must_use]
pub const fn exit_code(outcome: Outcome) -> i32 {
    if outcome.failed {
        return EXIT_ERROR;
    }
    if outcome.conflicts_remaining == 0 {
        return EXIT_SUCCESS;
    }
    if outcome.written {
        EXIT_CONFLICTS
    } else {
        EXIT_CONFLICTS_NOT_WRITTEN
    }
}

/// The line terminator the markers are written with.
///
/// The output follows the left input, so markers added to a file of one style
/// do not introduce a second one.
fn terminator(model: &MergeModel) -> &'static str {
    let lines = || {
        model
            .output_lines()
            .iter()
            .chain(model.inputs().left.iter())
    };
    match lines().find(|line| line.ends_with('\n')) {
        Some(line) if line.ends_with("\r\n") => "\r\n",
        None if lines().any(|line| line.ends_with('\r')) => "\r",
        Some(_) | None => "\n",
    }
}

/// Every line the output holds, with markers around each unresolved section.
///
/// A section that was decided contributes exactly what the output pane shows,
/// except that a marker after it never shares its last line.
#[must_use]
pub fn marked_text(model: &MergeModel, labels: &MarkerLabels) -> String {
    let end = terminator(model);
    let mut text = String::new();
    for (index, section) in model.sections().iter().enumerate() {
        if !section.is_unresolved_conflict() {
            if let Some(range) = model.output_range(index) {
                push_piece(&mut text, &model.output_text_range(range), end);
            }
            continue;
        }
        write_marker(&mut text, "<<<<<<< ", &labels.left, end);
        push(
            &mut text,
            &slice_of(&model.inputs().left, section.left.clone()),
            end,
        );
        if !model.inputs().two_way {
            write_marker(&mut text, "||||||| ", &labels.center, end);
            push(
                &mut text,
                &slice_of(&model.inputs().center, section.center.clone()),
                end,
            );
        }
        write_marker(&mut text, "=======", "", end);
        push(
            &mut text,
            &slice_of(&model.inputs().right, section.right.clone()),
            end,
        );
        write_marker(&mut text, ">>>>>>> ", &labels.right, end);
    }
    text
}

/// Compose markers around unresolved regions using immutable pane snapshots.
///
/// This is used by the save worker so a large output, or a large merge model,
/// is not copied on the UI frame thread.
pub(crate) fn marked_text_from_snapshot(
    output: &TextBuffer,
    left: &TextBuffer,
    center: Option<&TextBuffer>,
    right: &TextBuffer,
    plan: &MarkerSavePlan,
    labels: &MarkerLabels,
) -> String {
    let end = terminator_from_buffers(output, left);
    let output_len = output.len_lines();
    let mut text = String::new();
    let mut cursor = 0;
    for section in &plan.sections {
        let start = section.output.start.max(cursor).min(output_len);
        let finish = section.output.end.max(start).min(output_len);
        push_piece(
            &mut text,
            &output.line_range_text(LineRange::new(cursor, start)),
            end,
        );
        write_marker(&mut text, "<<<<<<< ", &labels.left, end);
        push_piece(
            &mut text,
            &left.line_range_text(LineRange::new(section.left.start, section.left.end)),
            end,
        );
        if !plan.two_way {
            write_marker(&mut text, "||||||| ", &labels.center, end);
            if let Some(center) = center {
                push_piece(
                    &mut text,
                    &center
                        .line_range_text(LineRange::new(section.center.start, section.center.end)),
                    end,
                );
            }
        }
        write_marker(&mut text, "=======", "", end);
        push_piece(
            &mut text,
            &right.line_range_text(LineRange::new(section.right.start, section.right.end)),
            end,
        );
        write_marker(&mut text, ">>>>>>> ", &labels.right, end);
        cursor = finish;
    }
    push_piece(
        &mut text,
        &output.line_range_text(LineRange::new(cursor, output_len)),
        end,
    );
    text
}

/// The marker line ending chosen by the model, found without flattening the
/// output or input buffers.
fn terminator_from_buffers(output: &TextBuffer, left: &TextBuffer) -> &'static str {
    let mut has_carriage_return = false;
    for buffer in [output, left] {
        for index in 0..buffer.len_lines() {
            let Some(line) = buffer.line(index) else {
                continue;
            };
            let length = line.len_chars();
            if length == 0 {
                continue;
            }
            match line.char(length - 1) {
                '\n' if length > 1 && line.char(length - 2) == '\r' => return "\r\n",
                '\n' => return "\n",
                '\r' => has_carriage_return = true,
                _ => {}
            }
        }
    }
    if has_carriage_return {
        "\r"
    } else {
        "\n"
    }
}

/// Write one marker line: the marker, the name it carries and a terminator.
fn write_marker(text: &mut String, marker: &str, label: &str, end: &str) {
    push_piece(text, &format!("{marker}{label}{end}"), end);
}

fn slice_of(lines: &[String], range: std::ops::Range<u32>) -> Vec<String> {
    let start = (range.start as usize).min(lines.len());
    let end = (range.end as usize).min(lines.len()).max(start);
    lines[start..end].to_vec()
}

fn push(text: &mut String, lines: &[String], end: &str) {
    for line in lines {
        push_piece(text, line, end);
    }
}

/// Append `piece` so that it starts a line of its own.
///
/// Unterminated text before it receives `end`, and a lone CR before a piece
/// that starts with LF receives an LF, because a CR followed by an LF reads
/// as one line ending.
fn push_piece(text: &mut String, piece: &str, end: &str) {
    if piece.is_empty() {
        return;
    }
    if !text.is_empty() && !text.ends_with(['\r', '\n']) {
        text.push_str(end);
    }
    if text.ends_with('\r') && piece.starts_with('\n') {
        text.push('\n');
    }
    text.push_str(piece);
}

/// The bytes the output file receives.
///
/// `template` supplies the encoding and byte order mark; it is the left input,
/// so the result is written the way the versions being merged were.
///
/// # Errors
///
/// Returns the encode failure rather than bytes that differ from the text.
pub fn to_bytes(
    model: &MergeModel,
    template: &LoadedText,
    markers: Option<&MarkerLabels>,
) -> Result<Vec<u8>, ca_text::SaveError> {
    encoded(model, template, markers).to_bytes()
}

/// True when a replacement character in the composed output may come from a
/// source that did not decode cleanly and therefore needs explicit consent.
#[must_use]
pub fn contains_lossy_input(model: &MergeModel, sources: &Sources) -> bool {
    let has_lossy_source = sources.left.had_errors
        || sources
            .center
            .as_ref()
            .is_some_and(|source| source.had_errors)
        || sources.right.had_errors;
    has_lossy_source
        && model
            .output_lines()
            .iter()
            .any(|line| line.contains('\u{fffd}'))
}

fn encoded(
    model: &MergeModel,
    template: &LoadedText,
    markers: Option<&MarkerLabels>,
) -> LoadedText {
    let text = markers.map_or_else(|| model.output_text(), |labels| marked_text(model, labels));
    let mut loaded = template.clone();
    loaded.buffer = TextBuffer::from_text(&text);
    // The caller checks source decode errors against the composed text and
    // obtains the necessary consent before clearing this input-side flag.
    loaded.had_errors = false;
    loaded
}

/// Write the output file, honoring every rule above.
///
/// `expected` is what the output looked like when the session first named it,
/// or [`Baseline::Unchecked`] when nothing is known about it.
pub fn save(
    files: &dyn FileSystem,
    path: &Path,
    model: &MergeModel,
    template: &LoadedText,
    markers: Option<&MarkerLabels>,
    expected: Baseline,
    consent: SaveConsent,
) -> SaveOutcome {
    save_with_endings(
        files, path, model, template, markers, expected, consent, None,
    )
}

/// Write the output file as [`save`] does, with every line ending rewritten to
/// `line_endings` when one is named.
#[allow(clippy::too_many_arguments)]
pub fn save_with_endings(
    files: &dyn FileSystem,
    path: &Path,
    model: &MergeModel,
    template: &LoadedText,
    markers: Option<&MarkerLabels>,
    expected: Baseline,
    consent: SaveConsent,
    line_endings: Option<ca_text::EolStyle>,
) -> SaveOutcome {
    let loaded = encoded(model, template, markers);
    ca_ui::save::text::save_with_endings(files, path, &loaded, expected, consent, line_endings)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        exit_code, marked_text, marked_text_from_snapshot, save, to_bytes, MarkerLabels,
        MarkerSavePlan, Outcome, EXIT_CONFLICTS, EXIT_CONFLICTS_NOT_WRITTEN, EXIT_ERROR,
        EXIT_SUCCESS,
    };
    use crate::model::{split, Inputs, MergeModel, Resolution};
    use ca_diff::merge3::MergeOptions;
    use ca_text::{DecodeOptions, LoadedText, TextBuffer};
    use ca_ui::save::text::SaveConsent;
    use ca_ui::save::{Baseline, FileSystem, RealFileSystem, SaveOutcome, Stamp};
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeFiles {
        content: Mutex<HashMap<PathBuf, Vec<u8>>>,
    }

    impl FakeFiles {
        fn bytes(&self, path: &Path) -> Option<Vec<u8>> {
            self.content.lock().ok()?.get(path).cloned()
        }

        fn put(&self, path: &Path, bytes: &[u8]) {
            if let Ok(mut map) = self.content.lock() {
                map.insert(path.to_path_buf(), bytes.to_vec());
            }
        }
    }

    impl FileSystem for FakeFiles {
        fn stamp(&self, path: &Path) -> Option<Stamp> {
            self.bytes(path).map(|bytes| Stamp {
                size: bytes.len() as u64,
                modified: None,
                identity: None,
            })
        }

        fn is_writable(&self, _path: &Path) -> bool {
            true
        }

        fn replace(&self, target: &Path, bytes: &[u8]) -> io::Result<()> {
            let Ok(mut map) = self.content.lock() else {
                return Err(io::Error::other("locked"));
            };
            map.insert(target.to_path_buf(), bytes.to_vec());
            Ok(())
        }
    }

    fn model(left: &str, center: &str, right: &str) -> MergeModel {
        MergeModel::build(
            Inputs {
                left: split(left),
                center: split(center),
                right: split(right),
                two_way: false,
            },
            &MergeOptions::default(),
            &ca_ui::worker::Cancel::new(),
        )
        .unwrap()
    }

    fn template() -> LoadedText {
        LoadedText::load(b"a\n", &DecodeOptions::default())
    }

    fn joined(lines: &[String]) -> String {
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
        }
        text
    }

    fn marked_snapshot(model: &MergeModel, labels: &MarkerLabels) -> String {
        let inputs = model.inputs();
        let output = TextBuffer::from_text(&model.output_text());
        let left = TextBuffer::from_text(&joined(&inputs.left));
        let center = (!inputs.two_way).then(|| TextBuffer::from_text(&joined(&inputs.center)));
        let right = TextBuffer::from_text(&joined(&inputs.right));
        marked_text_from_snapshot(
            &output,
            &left,
            center.as_ref(),
            &right,
            &MarkerSavePlan::from_model(model),
            labels,
        )
    }

    #[test]
    fn marker_save_snapshot_matches_the_model_for_line_endings_and_resolutions() {
        let labels = MarkerLabels {
            left: "left.txt".to_owned(),
            center: "base.txt".to_owned(),
            right: "right.txt".to_owned(),
        };
        let cases = [
            model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n"),
            model("a\rL\rc\r", "a\rb\rc\r", "a\rR\rc\r"),
            model("k\rx\rm\r", "k\rc\rm\r", "k\n\nm\n"),
        ];
        for merged in cases {
            assert_eq!(
                marked_snapshot(&merged, &labels),
                marked_text(&merged, &labels),
            );
        }

        let two_way = MergeModel::build(
            Inputs {
                left: split("L\n"),
                center: Vec::new(),
                right: split("R\n"),
                two_way: true,
            },
            &MergeOptions::default(),
            &ca_ui::worker::Cancel::new(),
        )
        .unwrap();
        assert_eq!(
            marked_snapshot(&two_way, &labels),
            marked_text(&two_way, &labels),
        );

        let mut resolved = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let section = resolved
            .sections()
            .iter()
            .position(crate::model::Section::is_unresolved_conflict)
            .unwrap();
        resolved.set_resolution(section, Resolution::Left);
        assert_eq!(
            marked_snapshot(&resolved, &labels),
            marked_text(&resolved, &labels),
        );
    }

    #[test]
    fn a_resolved_merge_writes_the_output_the_pane_shows() {
        let merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nb\nc\n");
        let bytes = to_bytes(&merged, &template(), None).unwrap();
        assert_eq!(bytes, b"a\nL\nc\n");
    }

    #[test]
    fn an_unresolved_conflict_is_written_between_markers() {
        let merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let text = marked_text(&merged, &MarkerLabels::default());
        assert_eq!(
            text,
            "a\n<<<<<<< left\nL\n||||||| center\nb\n=======\nR\n>>>>>>> right\nc\n"
        );
    }

    #[test]
    fn a_two_way_merge_writes_markers_without_an_ancestor_block() {
        let merged = MergeModel::build(
            Inputs {
                left: split("L\n"),
                center: Vec::new(),
                right: split("R\n"),
                two_way: true,
            },
            &MergeOptions::default(),
            &ca_ui::worker::Cancel::new(),
        )
        .unwrap();
        let text = marked_text(&merged, &MarkerLabels::default());
        assert_eq!(text, "<<<<<<< left\nL\n=======\nR\n>>>>>>> right\n");
    }

    #[test]
    fn an_opening_marker_after_an_unterminated_line_starts_its_own_line() {
        let merged = model("a\nb\nL\n", "a\nb", "a\nb\nR\n");
        assert_eq!(merged.output_text(), "a\nb");
        assert_eq!(
            marked_text(&merged, &MarkerLabels::default()),
            "a\nb\n<<<<<<< left\nL\n||||||| center\n=======\nR\n>>>>>>> right\n"
        );
    }

    #[test]
    fn a_block_ending_in_a_lone_carriage_return_gains_no_blank_line() {
        let merged = model("a\r\nL\rc\r\n", "a\r\nb\r\nc\r\n", "a\r\nR\r\nc\r\n");
        assert_eq!(
            marked_text(&merged, &MarkerLabels::default()),
            "a\r\n<<<<<<< left\r\nL\r||||||| center\r\nb\r\n=======\r\nR\r\n>>>>>>> right\r\nc\r\n"
        );
    }

    #[test]
    fn markers_in_a_carriage_return_file_keep_its_one_line_ending_style() {
        let merged = model("a\rL\rc\r", "a\rb\rc\r", "a\rR\rc\r");
        assert_eq!(
            marked_text(&merged, &MarkerLabels::default()),
            "a\r<<<<<<< left\rL\r||||||| center\rb\r=======\rR\r>>>>>>> right\rc\r"
        );
        let merged = model("k\rx\rm\r", "k\rc\rm\r", "k\n\nm\n");
        assert_eq!(
            marked_text(&merged, &MarkerLabels::default()),
            "k\r<<<<<<< left\rx\r||||||| center\rc\r=======\r\n\n>>>>>>> right\rm\r"
        );
    }

    #[test]
    fn a_decided_conflict_is_written_without_markers() {
        let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let section = merged
            .sections()
            .iter()
            .position(crate::model::Section::is_unresolved_conflict)
            .unwrap();
        merged.set_resolution(section, Resolution::Left);
        assert_eq!(marked_text(&merged, &MarkerLabels::default()), "a\nL\nc\n");
    }

    #[test]
    fn real_file_system_replaces_the_target_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        std::fs::write(&target, b"previous\n").unwrap();
        let files = RealFileSystem;
        let expected = Baseline::of(&files, &target);
        let merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nb\nc\n");
        let outcome = save(
            &files,
            &target,
            &merged,
            &template(),
            None,
            expected,
            SaveConsent::default(),
        );
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_eq!(std::fs::read(&target).unwrap(), b"a\nL\nc\n");
        let mut entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        entries.sort();
        assert_eq!(entries, vec![std::ffi::OsString::from("out.txt")]);
    }

    #[test]
    fn an_output_that_changed_on_disk_is_not_overwritten() {
        let files = FakeFiles::default();
        let target = PathBuf::from("/work/out.txt");
        files.put(&target, b"older\n");
        let stamp = Baseline::of(&files, &target);
        files.put(&target, b"someone else wrote this\n");
        let merged = model("a\n", "a\n", "a\n");
        let outcome = save(
            &files,
            &target,
            &merged,
            &template(),
            None,
            stamp,
            SaveConsent::default(),
        );
        assert_eq!(outcome, SaveOutcome::ChangedOnDisk);
        assert_eq!(
            files.bytes(&target).as_deref(),
            Some(b"someone else wrote this\n".as_ref())
        );
    }

    #[test]
    fn saving_with_conflicts_writes_the_markers_the_user_agreed_to() {
        let files = FakeFiles::default();
        let target = PathBuf::from("/work/out.txt");
        let merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let labels = MarkerLabels::default();
        let outcome = save(
            &files,
            &target,
            &merged,
            &template(),
            Some(&labels),
            Baseline::Unchecked,
            SaveConsent::default(),
        );
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        let written = String::from_utf8(files.bytes(&target).unwrap()).unwrap();
        assert!(written.contains("<<<<<<< left"));
        assert!(written.contains(">>>>>>> right"));
    }

    #[test]
    fn an_encoding_that_cannot_hold_the_output_is_reported_rather_than_written() {
        let files = FakeFiles::default();
        let target = PathBuf::from("/work/out.txt");
        let options = DecodeOptions {
            forced: Some(ca_text::TextEncoding::Legacy(encoding_rs::WINDOWS_1252)),
            ..DecodeOptions::default()
        };
        let template = LoadedText::load(b"a\n", &options);
        let merged = model("\u{4e2d}\n", "a\n", "a\n");
        let outcome = save(
            &files,
            &target,
            &merged,
            &template,
            None,
            Baseline::Unchecked,
            SaveConsent::default(),
        );
        assert!(matches!(outcome, SaveOutcome::WouldLose(_)));
        assert!(files.bytes(&target).is_none());
    }

    #[test]
    fn the_exit_code_follows_what_the_merge_ended_with() {
        let table = [
            (0, true, false, EXIT_SUCCESS),
            (0, false, false, EXIT_SUCCESS),
            (2, true, false, EXIT_CONFLICTS),
            (2, false, false, EXIT_CONFLICTS_NOT_WRITTEN),
            (0, true, true, EXIT_ERROR),
        ];
        for (conflicts_remaining, written, failed, expected) in table {
            assert_eq!(
                exit_code(Outcome {
                    conflicts_remaining,
                    written,
                    failed,
                }),
                expected,
                "{conflicts_remaining} {written} {failed}"
            );
        }
    }
}
