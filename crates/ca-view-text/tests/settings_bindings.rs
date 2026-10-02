//! Every field of the text comparison settings changes a comparison result.
//!
//! Each test sets one field, runs the comparison on a small fixture written to
//! a temporary folder, and asserts an outcome the default settings do not
//! produce. A field with no test here is a field nothing binds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::settings::common::{EncodingChoice, FileFormatChoice, ReplacementItem, RuleSide};
use ca_session::settings::text::{element, ManualAlignment, TextCompareSettings};
use ca_ui::theme::{palette, Variant};
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::TextView;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn context() -> ViewContext {
    ViewContext {
        options: Arc::default(),
        palette: palette(Variant::Light),
        notify: Arc::new(|| {}),
    }
}

fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1_100.0, 460.0),
        )),
        ..egui::RawInput::default()
    }
}

/// Counts of one comparison, which is what a bound field has to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    differences: usize,
    important: usize,
    unimportant: usize,
    rows: usize,
}

struct Harness {
    view: TextView,
    ctx: egui::Context,
    context: ViewContext,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}

impl Harness {
    fn bytes(name: &str, left: &[u8], right: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join(format!("left.{name}"));
        let right_path = dir.path().join(format!("right.{name}"));
        std::fs::write(&left_path, left).unwrap();
        std::fs::write(&right_path, right).unwrap();
        let context = context();
        let view = TextView::new(left_path, right_path, &context, 1);
        let mut harness = Self {
            view,
            ctx: egui::Context::default(),
            context,
            dir,
        };
        harness.settle();
        harness
    }

    fn new(name: &str, left: &str, right: &str) -> Self {
        Self::bytes(name, left.as_bytes(), right.as_bytes())
    }

    fn frame(&mut self) {
        self.view.tick();
        let view = &mut self.view;
        let context = &self.context;
        let _ = self.ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, context);
            });
        });
    }

    /// Paint frames until the comparison is ready again.
    ///
    /// The view schedules a re-diff when its settings change, so the first
    /// frames still describe the previous run. Waiting on the view's own state
    /// rather than on a clock is what keeps the test from racing the worker.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.frame();
            if self.view.is_settled() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the comparison never finished");
    }

    fn apply(&mut self, settings: TextCompareSettings) -> Counts {
        self.view.apply_session_settings(settings);
        self.settle();
        self.counts()
    }

    fn counts(&self) -> Counts {
        let counts = self.view.model().counts();
        Counts {
            differences: counts.differences,
            important: counts.important,
            unimportant: counts.unimportant,
            rows: self.view.model().rows().len(),
        }
    }

    fn settings(&self) -> TextCompareSettings {
        self.view.session_settings().clone()
    }
}

/// Every entry of the grammar element checklist changes a comparison.
///
/// One fixture carries a difference inside each class at once; unchecking one
/// class moves exactly that difference to unimportant, so the counts differ
/// from the default run for every entry.
#[test]
fn every_grammar_element_class_changes_a_comparison() {
    let cases: [(&str, &str, &str); 4] = [
        (
            element::COMMENT,
            "let a = 1; // first\n",
            "let a = 1; // second\n",
        ),
        (
            element::STRING,
            "let a = \"first\";\n",
            "let a = \"second\";\n",
        ),
        (element::NUMBER, "let a = 11;\n", "let a = 22;\n"),
        (element::KEYWORD, "let a = f();\n", "const a = f();\n"),
    ];
    for (class, left, right) in cases {
        let mut harness = Harness::new("rs", left, right);
        let before = harness.counts();
        assert_eq!(before.important, 1, "{class}: the fixture starts important");

        let mut settings = harness.settings();
        settings.importance.orphan_lines_always_important = false;
        settings.importance.everything_else_important = false;
        settings.importance.set_element_important(class, false);
        let after = harness.apply(settings);
        assert_eq!(
            after.important, 0,
            "{class} stayed important after it was unchecked"
        );
        assert_eq!(after.unimportant, 1, "{class} lost its difference entirely");
    }
}

/// Unchecking one class leaves the others alone, so the checklist is five
/// switches and not one.
#[test]
fn unchecking_one_class_leaves_another_class_important() {
    let mut harness = Harness::new("rs", "let a = 1; // note\n", "let b = 2; // memo\n");
    let mut settings = harness.settings();
    settings.importance.orphan_lines_always_important = false;
    settings.importance.everything_else_important = false;
    settings
        .importance
        .set_element_important(element::COMMENT, false);
    let after = harness.apply(settings);
    assert_eq!(
        after.important, 1,
        "the identifier and number changes still count"
    );
}

#[test]
fn a_replacement_makes_a_matching_difference_unimportant() {
    let mut harness = Harness::new("txt", "an apple a day\n", "an orange a day\n");
    assert_eq!(harness.counts().important, 1);

    let mut settings = harness.settings();
    settings.replacements.items.push(ReplacementItem {
        find: "apple".to_owned(),
        replace_with: "orange".to_owned(),
        whole_words_only: true,
        side: RuleSide::Left,
        ..ReplacementItem::default()
    });
    let after = harness.apply(settings);
    assert_eq!(after.important, 0);
    assert_eq!(after.unimportant, 1);
}

#[test]
fn a_replacement_does_not_hide_an_unrelated_difference() {
    let mut harness = Harness::new("txt", "an apple a day\n", "an orange a week\n");
    let mut settings = harness.settings();
    settings.replacements.items.push(ReplacementItem {
        find: "apple".to_owned(),
        replace_with: "orange".to_owned(),
        side: RuleSide::Left,
        ..ReplacementItem::default()
    });
    assert_eq!(harness.apply(settings).important, 1);
}

#[test]
fn a_manual_pairing_changes_the_row_layout() {
    let mut harness = Harness::new("txt", "x\na\nb\n", "a\nb\nx\n");
    let before = harness.counts();

    let mut settings = harness.settings();
    settings.alignment.manual_alignments.push(ManualAlignment {
        left_start: 0,
        left_end: 1,
        right_start: 0,
        right_end: 1,
        ..ManualAlignment::default()
    });
    let after = harness.apply(settings);
    assert_ne!(
        after, before,
        "forcing line one against line one produced the same layout"
    );
}

/// A forced pairing of nothing stands a run alone, which is what the isolate
/// command asks the engine for.
#[test]
fn a_pairing_against_nothing_stands_a_run_alone() {
    let mut harness = Harness::new("txt", "a\nb\nc\n", "a\nb\nc\n");
    assert_eq!(harness.counts().differences, 0);

    let mut settings = harness.settings();
    settings.alignment.manual_alignments.push(ManualAlignment {
        left_start: 1,
        left_end: 2,
        right_start: 1,
        right_end: 1,
        ..ManualAlignment::default()
    });
    assert!(
        harness.apply(settings).differences > 0,
        "the isolated line still paired with its twin"
    );
}

#[test]
fn an_alignment_algorithm_reaches_the_comparison() {
    let mut harness = Harness::new("txt", "a\nb\nc\nd\n", "b\nc\nd\ne\n");
    let before = harness.counts();
    let mut settings = harness.settings();
    settings.alignment.algorithm = ca_session::settings::common::AlignmentAlgorithm::Unaligned;
    assert_ne!(harness.apply(settings), before);
}

#[test]
fn never_aligning_differences_changes_the_row_layout() {
    let mut harness = Harness::new("txt", "same\nalpha\nsame\n", "same\nbeta\nsame\n");
    let before = harness.counts();
    let mut settings = harness.settings();
    settings.alignment.never_align_differences = true;
    let after = harness.apply(settings);
    assert!(
        after.rows > before.rows,
        "a changed pair was not split into two blocks"
    );
}

/// The body of the right side sits eight lines further down than the body of
/// the left, so a bound under eight rejects the match a wider bound accepts.
#[test]
fn a_skew_bound_changes_the_comparison() {
    use std::fmt::Write as _;
    let mut body = String::new();
    for index in 0..40 {
        let _ = writeln!(body, "line {index}");
    }
    let left = body.clone();
    let mut right = String::new();
    for index in 0..8 {
        let _ = writeln!(right, "head {index}");
    }
    right.push_str(&body);

    let mut harness = Harness::new("txt", &left, &right);
    let mut settings = harness.settings();
    settings.alignment.use_closeness_matching = false;
    settings.alignment.skew_tolerance = 50;
    let loose = harness.apply(settings.clone());
    settings.alignment.skew_tolerance = 1;
    let tight = harness.apply(settings);
    assert_ne!(
        loose, tight,
        "the skew bound did not reach the alignment pass"
    );
}

#[test]
fn closeness_matching_changes_the_comparison() {
    let mut harness = Harness::new("txt", "aaa bbb ccc\nzzz\n", "aaa bbb cdd\nyyy\n");
    let before = harness.counts();
    let mut settings = harness.settings();
    settings.alignment.use_closeness_matching = false;
    settings.alignment.never_align_differences = true;
    assert_ne!(harness.apply(settings), before);
}

#[test]
fn comparing_line_endings_changes_the_comparison() {
    let mut harness = Harness::bytes("txt", b"one\r\ntwo\r\n", b"one\ntwo\n");
    assert_eq!(harness.counts().differences, 0);
    let mut settings = harness.settings();
    settings.importance.compare_line_endings = true;
    let counts = harness.apply(settings);
    assert!(counts.differences > 0);
    assert_eq!(counts.important, counts.differences);
    assert_eq!(counts.unimportant, 0);
}

/// A pinned encoding decodes bytes the detector reads another way, so the two
/// sides stop matching.
#[test]
fn a_pinned_encoding_reaches_the_loader() {
    let utf16: Vec<u8> = "one\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut harness = Harness::bytes("txt", &utf16, b"one\n");
    assert_eq!(
        harness.counts().differences,
        0,
        "detection reads the two sides as the same text"
    );
    let mut settings = harness.settings();
    settings.format.left_encoding = EncodingChoice::Named {
        name: "UTF-8".to_owned(),
        unknown: std::collections::BTreeMap::new(),
    };
    assert!(
        harness.apply(settings).differences > 0,
        "the pinned encoding never reached the decoder"
    );
}

#[test]
fn western_encoded_accented_bytes_compare_as_different() {
    let harness = Harness::bytes("txt", b"caf\xE9\n", b"caf\xE8\n");

    assert!(
        harness.counts().differences > 0,
        "distinct Windows-1252 characters were decoded to equal replacement text"
    );
}

#[test]
fn a_missing_final_newline_is_same_with_default_settings() {
    let harness = Harness::new("txt", "a\nb", "a\nb\n");

    assert_eq!(harness.counts().differences, 0);
}

#[test]
fn hiding_unimportant_content_leaves_a_status_notice() {
    let mut harness = Harness::new(
        "rs",
        "let value = 1; // before\n",
        "let value = 1; // after\n",
    );
    let mut settings = harness.settings();
    settings.importance.orphan_lines_always_important = false;
    settings.importance.everything_else_important = false;
    settings
        .importance
        .set_element_important(element::COMMENT, false);
    let counts = harness.apply(settings);
    assert_eq!(counts.unimportant, 1);

    harness.view.set_ignore_unimportant(true);

    assert!(harness
        .view
        .status_fields()
        .iter()
        .any(|field| field == "Ignoring 1 unimportant difference(s)"));
}

/// A pinned format decides the grammar, so an element the file name's own
/// format does not recognise becomes unimportant under it.
#[test]
fn a_pinned_format_reaches_the_grammar() {
    let mut harness = Harness::new("txt", "x = 1 # first\n", "x = 1 # second\n");
    let mut settings = harness.settings();
    settings.importance.orphan_lines_always_important = false;
    settings.importance.everything_else_important = false;
    settings
        .importance
        .set_element_important(element::COMMENT, false);
    assert_eq!(
        harness.apply(settings.clone()).important,
        1,
        "plain text has no comment element, so the change still counts"
    );

    settings.format.left_format = FileFormatChoice::Named {
        name: "Python".to_owned(),
        unknown: std::collections::BTreeMap::new(),
    };
    assert_eq!(
        harness.apply(settings).important,
        0,
        "the pinned format's comment element never reached the rules"
    );
}

/// The toolbar's rule menu and the settings dialog edit one stored value.
#[test]
fn the_toolbar_rules_and_the_stored_settings_are_one_value() {
    let harness = Harness::new("rs", "// a\n", "// b\n");
    let mut view = harness.view;
    let mut rules = view.rules();
    assert!(rules.elements.comments);
    rules.elements.comments = false;
    rules.set_whitespace_unimportant(true);
    rules.case_unimportant = false;
    view.set_rules(rules);

    let stored = view.session_settings();
    assert!(!stored.importance.element_important(element::COMMENT));
    assert!(!stored.importance.leading_whitespace_important);
    assert!(!stored.importance.embedded_whitespace_important);
    assert!(!stored.importance.trailing_whitespace_important);
    assert!(stored.importance.character_case_important);
    assert_eq!(view.rules(), rules, "the stored value did not round trip");
}

/// A settings edit made through the dialog reaches the toolbar's reading of
/// the same value, which is the other direction of the same rule.
#[test]
fn a_settings_edit_reaches_the_toolbar_rules() {
    let harness = Harness::new("rs", "// a\n", "// b\n");
    let mut view = harness.view;
    let mut settings = view.session_settings().clone();
    settings
        .importance
        .set_element_important(element::NUMBER, false);
    view.apply_session_settings(settings);
    assert!(!view.rules().elements.numbers);
}
