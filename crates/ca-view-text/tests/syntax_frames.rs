//! Real frames over a source file: which format is chosen, what color each run
//! gets, and how the importance rules change the counts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_grammar::StyleSlot;
use ca_ui::theme::{palette, Variant};
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::jobs::RuleToggles;
use ca_view_text::sidecopy::Side;
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

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1_100.0, 460.0),
        )),
        events,
        ..egui::RawInput::default()
    }
}

struct Harness {
    view: TextView,
    ctx: egui::Context,
    context: ViewContext,
    dir: tempfile::TempDir,
}

impl Harness {
    fn new(name: &str, left: &str, right: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join(format!("left.{name}"));
        let right_path = dir.path().join(format!("right.{name}"));
        std::fs::write(&left_path, left.as_bytes()).unwrap();
        std::fs::write(&right_path, right.as_bytes()).unwrap();
        let context = context();
        let view = TextView::new(left_path, right_path, &context, 1);
        let mut harness = Self {
            view,
            ctx: egui::Context::default(),
            context,
            dir,
        };
        harness.run_until(TextView::is_ready);
        harness
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.view.tick();
        let view = &mut self.view;
        let context = &self.context;
        let _ = self.ctx.run(raw_input(events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, context);
            });
        });
    }

    /// Paint frames until `done` holds, which is how a test waits for work a
    /// worker is doing.
    fn run_until(&mut self, done: impl Fn(&TextView) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame(Vec::new());
            if done(&self.view) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the work never finished");
    }

    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.frame(vec![egui::Event::Text(character.to_string())]);
        }
    }

    fn slot_at(&mut self, side: Side, line: u32, column: u32) -> Option<StyleSlot> {
        let spans = self.view.syntax_spans(side, line)?;
        spans
            .iter()
            .find(|span| span.start <= column && column < span.end)
            .map(|span| span.slot)
    }
}

const SAMPLE: &str = "fn main() {\n    let count = 12; // how many\n    println!(\"hello\");\n}\n";

#[test]
fn a_rust_file_name_selects_the_rust_format() {
    let harness = Harness::new("rs", SAMPLE, SAMPLE);
    assert_eq!(harness.view.format_name(), "Rust");
}

#[test]
fn each_run_of_a_rust_line_takes_its_own_color() {
    let mut harness = Harness::new("rs", SAMPLE, SAMPLE);
    harness.frame(Vec::new());
    let table = palette(Variant::Light);
    // `fn` on line one, the comment on line two, the string on line three.
    assert_eq!(harness.slot_at(Side::Left, 0, 0), Some(StyleSlot::Keyword));
    assert_eq!(
        harness.slot_at(Side::Left, 1, 20),
        Some(StyleSlot::Comment),
        "the comment run is colored as a comment"
    );
    assert_eq!(harness.slot_at(Side::Left, 2, 14), Some(StyleSlot::Literal));
    assert_ne!(
        table.syntax_slot(StyleSlot::Comment),
        table.syntax_slot(StyleSlot::Keyword)
    );
    for variant in [Variant::Light, Variant::Dark] {
        let table = palette(variant);
        assert_ne!(table.syntax_slot(StyleSlot::Comment), table.same_line);
    }
}

#[test]
fn a_plain_text_file_has_no_language_elements() {
    let mut harness = Harness::new("txt", "one // two\n", "one // two\n");
    harness.frame(Vec::new());
    assert_ne!(harness.view.format_name(), "Rust");
    let spans = harness.view.syntax_spans(Side::Left, 0).unwrap();
    assert!(
        spans
            .iter()
            .all(|span| span.slot != StyleSlot::Comment && span.slot != StyleSlot::Keyword),
        "the fallback format claims no comments and no keywords"
    );
}

#[test]
fn opening_a_block_comment_recolors_the_lines_under_it() {
    let text = "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;\n";
    let mut harness = Harness::new("rs", text, text);
    harness.run_until(TextView::syntax_converged);
    assert_ne!(harness.slot_at(Side::Left, 2, 0), Some(StyleSlot::Comment));
    // The caret starts on line one, above every line the assertion reads.
    harness.type_text("/*");
    harness.run_until(TextView::syntax_converged);
    harness.frame(Vec::new());
    assert_eq!(harness.slot_at(Side::Left, 2, 0), Some(StyleSlot::Comment));
    assert_eq!(harness.slot_at(Side::Left, 3, 0), Some(StyleSlot::Comment));
}

#[test]
fn a_comment_only_change_follows_the_comment_rule() {
    let left = "fn main() {}\n// first note\n";
    let right = "fn main() {}\n// second note\n";
    let mut harness = Harness::new("rs", left, right);
    let counts = harness.view.model().counts();
    assert_eq!(counts.differences, 1);
    assert_eq!(counts.important, 1, "comments are important by default");
    assert_eq!(counts.unimportant, 0);

    harness.view.set_rules(RuleToggles {
        elements: ca_view_text::jobs::ElementRules {
            comments: false,
            ..ca_view_text::jobs::ElementRules::default()
        },
        ..RuleToggles::default()
    });
    harness.run_until(|view| view.model().counts().unimportant > 0);
    let counts = harness.view.model().counts();
    assert_eq!(counts.differences, 1);
    assert_eq!(counts.important, 0);
    assert_eq!(counts.unimportant, 1);

    harness.view.set_rules(RuleToggles::default());
    harness.run_until(|view| view.model().counts().important > 0);
    assert_eq!(harness.view.model().counts().unimportant, 0);
}

#[test]
fn saving_under_a_new_name_writes_the_file_and_follows_its_format() {
    let mut harness = Harness::new("txt", "one\n", "one\n");
    assert_ne!(harness.view.format_name(), "Rust");
    harness.type_text("fn ");
    let target = harness.dir.path().join("renamed.rs");
    harness.view.save_active_as(&target);
    harness.run_until(|view| !view.pane(Side::Left).buffer().is_modified());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn one\n");
    assert_eq!(harness.view.format_name(), "Rust");
    harness.run_until(TextView::syntax_converged);
    assert_eq!(harness.slot_at(Side::Left, 0, 0), Some(StyleSlot::Keyword));
}

#[test]
fn a_whitespace_only_change_follows_the_whitespace_rule() {
    let left = "fn main() {}\nlet a = 1;\n";
    let right = "fn main() {}\nlet  a  =  1;\n";
    let mut harness = Harness::new("rs", left, right);
    assert_eq!(harness.view.model().counts().important, 1);
    let mut rules = RuleToggles::default();
    rules.set_whitespace_unimportant(true);
    harness.view.set_rules(rules);
    harness.run_until(|view| view.model().counts().unimportant > 0);
    assert_eq!(harness.view.model().counts().important, 0);
}
