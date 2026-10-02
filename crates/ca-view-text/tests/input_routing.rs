//! Frames that click and type, checking that exactly one pane owns the caret
//! and that every character lands once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::command::{self, Keystroke};
use ca_ui::theme::{palette, Variant};
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::sidecopy::Side;
use ca_view_text::TextView;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A window size at which a click can land on either pane.
const WINDOW: [f32; 2] = [1_100.0, 460.0];

/// Left file of the pair.
const LEFT: &str = "alpha\nbeta\ngamma\ndelta\n";
/// Right file of the pair, one line longer.
const RIGHT: &str = "alpha\nBETA\ngamma\nepsilon\ndelta\n";

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
            egui::vec2(WINDOW[0], WINDOW[1]),
        )),
        events,
        ..egui::RawInput::default()
    }
}

struct Harness {
    view: TextView,
    ctx: egui::Context,
    context: ViewContext,
    /// A field that holds the keyboard, as a live text field does every frame.
    focus: Option<egui::Id>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new(left: &str, right: &str) -> Self {
        Self::named_with_read_only("rs", left, right, false)
    }

    fn named(suffix: &str, left: &str, right: &str) -> Self {
        Self::named_with_read_only(suffix, left, right, false)
    }

    fn read_only(left: &str, right: &str) -> Self {
        Self::named_with_read_only("rs", left, right, true)
    }

    fn named_with_read_only(suffix: &str, left: &str, right: &str, read_only: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join(format!("left.{suffix}"));
        let right_path = dir.path().join(format!("right.{suffix}"));
        std::fs::write(&left_path, left.as_bytes()).unwrap();
        std::fs::write(&right_path, right.as_bytes()).unwrap();
        let context = context();
        let view = if read_only {
            use ca_ui::view::ViewFactory;
            let request = ca_ui::view::OpenRequest::new(
                ca_session::SessionKind::TextCompare,
                left_path,
                right_path,
            )
            .over_temporaries(Vec::new());
            TextView::create_from(&request, &context, 1)
        } else {
            TextView::new(left_path, right_path, &context, 1)
        };
        let mut harness = Self {
            view,
            ctx: egui::Context::default(),
            context,
            focus: None,
            _dir: dir,
        };
        harness.run_until_ready();
        harness
    }

    /// One frame, with the shell's own keyboard routing in front of the view so
    /// the test covers both handlers a real window runs.
    fn frame(&mut self, events: Vec<egui::Event>) {
        self.view.tick();
        if let Some(id) = self.focus {
            self.ctx.memory_mut(|memory| memory.request_focus(id));
        }
        let view = &mut self.view;
        let context = &self.context;
        let _ = self.ctx.run(raw_input(events), |ctx| {
            let strokes: Vec<Keystroke> = ctx.input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => Some(Keystroke {
                            key: *key,
                            command: modifiers.command,
                            shift: modifiers.shift,
                            alt: modifiers.alt,
                        }),
                        _ => None,
                    })
                    .collect()
            });
            for stroke in strokes {
                if let Some(found) = command::route(ca_ui::command::MenuView::Text, stroke) {
                    if view.accepts(found) {
                        view.run(found);
                    }
                }
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, context);
            });
        });
    }

    fn run_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame(Vec::new());
            if self.view.is_ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the comparison never became ready");
    }

    /// The centre of the text row at `row`, counted from the top of the rows.
    fn row_point(&self, x: f32, row: usize) -> egui::Pos2 {
        let body = self.view.body_rect();
        #[allow(clippy::cast_precision_loss)]
        let offset = row as f32;
        let height = self.view.row_pixels();
        egui::pos2(x, offset.mul_add(height, body.top()) + height / 2.0)
    }

    /// One primary click, pressed and released.
    fn click(&mut self, at: egui::Pos2) {
        self.frame(vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ]);
        self.frame(vec![egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }]);
    }

    /// Type text the way a desktop backend delivers it: the key press and the
    /// character, in the same frame.
    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            let name = if character == ' ' {
                "Space".to_owned()
            } else {
                character.to_uppercase().to_string()
            };
            let mut events = Vec::new();
            if let Some(key) = egui::Key::from_name(&name) {
                events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                });
            }
            events.push(egui::Event::Text(character.to_string()));
            self.frame(events);
        }
    }

    fn press(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]);
    }

    fn text(&self, side: Side) -> String {
        self.view.pane(side).buffer().text()
    }
}

#[test]
fn a_click_in_the_left_pane_on_a_gap_row_edits_only_the_left_pane() {
    let mut harness = Harness::new(LEFT, RIGHT);
    // Visual row four shows a gap on the left and `epsilon` on the right.
    let point = harness.row_point(300.0, 3);
    assert!(point.x < harness.view.pane_split_x());
    harness.click(point);
    assert_eq!(harness.view.active_side(), Side::Left);
    // The nearest real line above the gap is `gamma`.
    assert_eq!(harness.view.pane(Side::Left).caret().line, 2);
    harness.press(egui::Key::End, egui::Modifiers::default());
    harness.type_text(" typed by test");
    assert_eq!(
        harness.text(Side::Left),
        "alpha\nbeta\ngamma typed by test\ndelta\n"
    );
    assert_eq!(harness.text(Side::Right), RIGHT);
    assert!(!harness.view.pane(Side::Right).buffer().is_modified());
}

#[test]
fn a_click_on_a_gap_row_in_the_right_pane_stays_in_the_right_pane() {
    // The left file is the longer one here, so the gap falls on the right.
    let mut harness = Harness::new(RIGHT, LEFT);
    let split = harness.view.pane_split_x();
    let point = harness.row_point(split + 120.0, 3);
    harness.click(point);
    assert_eq!(harness.view.active_side(), Side::Right);
    assert_eq!(harness.view.pane(Side::Right).caret().line, 2);
    harness.type_text("Z");
    assert_eq!(harness.text(Side::Left), RIGHT);
    assert!(harness.text(Side::Right).contains("Zgamma"));
}

#[test]
fn a_whole_sentence_lands_once_in_one_buffer() {
    let sentence = "the quick brown fox jumps over the lazy dog";
    let mut harness = Harness::new("start\n", "start\n");
    harness.click(harness.row_point(300.0, 0));
    assert_eq!(harness.view.active_side(), Side::Left);
    harness.press(egui::Key::End, egui::Modifiers::default());
    harness.type_text(sentence);
    assert_eq!(harness.text(Side::Left), format!("start{sentence}\n"));
    assert_eq!(harness.text(Side::Right), "start\n");
    let seen = harness.text(Side::Left).matches(sentence).count();
    assert_eq!(seen, 1, "the sentence was inserted {seen} times");
}

#[test]
fn a_shortcut_letter_never_becomes_text() {
    let mut harness = Harness::new("start\n", "start\n");
    let command = egui::Modifiers {
        command: true,
        ctrl: true,
        ..egui::Modifiers::default()
    };
    for letter in ["A", "D", "F", "G", "L", "N", "P", "R", "S", "U", "Z"] {
        let Some(key) = egui::Key::from_name(letter) else {
            continue;
        };
        harness.frame(vec![
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: command,
            },
            // Some backends deliver the letter of a shortcut as text as well.
            egui::Event::Text(letter.to_lowercase()),
        ]);
    }
    assert_eq!(harness.text(Side::Left), "start\n");
    assert_eq!(harness.text(Side::Right), "start\n");
}

#[test]
fn typing_after_a_click_never_reaches_the_other_pane() {
    let mut harness = Harness::new(LEFT, RIGHT);
    let split = harness.view.pane_split_x();
    harness.click(harness.row_point(split + 80.0, 0));
    assert_eq!(harness.view.active_side(), Side::Right);
    harness.type_text("right");
    assert_eq!(harness.text(Side::Left), LEFT);
    assert!(!harness.view.pane(Side::Left).buffer().is_modified());

    harness.click(harness.row_point(100.0, 0));
    assert_eq!(harness.view.active_side(), Side::Left);
    let before = harness.text(Side::Right);
    harness.type_text("left");
    assert_eq!(harness.text(Side::Right), before);
}

#[test]
fn a_word_delete_stops_at_a_token_edge() {
    let mut harness = Harness::new("let alpha_one = 12;\n", "let alpha_one = 12;\n");
    assert_eq!(harness.view.format_name(), "Rust");
    let command = egui::Modifiers {
        command: true,
        ctrl: true,
        ..egui::Modifiers::default()
    };
    harness.press(egui::Key::End, egui::Modifiers::default());
    // The caret sits past the semicolon; the first delete takes it, the second
    // takes the number.
    harness.press(egui::Key::Backspace, command);
    assert_eq!(harness.text(Side::Left), "let alpha_one = 12\n");
    harness.press(egui::Key::Backspace, command);
    assert_eq!(harness.text(Side::Left), "let alpha_one = \n");
    assert_eq!(harness.text(Side::Right), "let alpha_one = 12;\n");
}

#[test]
fn a_word_delete_forward_uses_word_boundaries_without_a_format() {
    let mut harness = Harness::named("txt", "alpha beta\n", "alpha beta\n");
    let command = egui::Modifiers {
        command: true,
        ctrl: true,
        ..egui::Modifiers::default()
    };
    harness.press(egui::Key::Delete, command);
    assert_ne!(harness.text(Side::Left), "alpha beta\n");
    assert!(harness.text(Side::Left).ends_with("beta\n"));
}

#[test]
fn edit_navigation_visits_the_lines_that_were_typed_into() {
    let mut harness = Harness::new("one\ntwo\nthree\nfour\n", "one\ntwo\nthree\nfour\n");
    assert!(!harness.view.accepts(command::Command::NextEdit));
    harness.click(harness.row_point(100.0, 2));
    harness.type_text("X");
    harness.click(harness.row_point(100.0, 0));
    assert!(harness.view.accepts(command::Command::NextEdit));
    harness.view.run(command::Command::NextEdit);
    assert_eq!(harness.view.pane(Side::Left).caret().line, 2);
    harness.click(harness.row_point(100.0, 3));
    harness.view.run(command::Command::PreviousEdit);
    assert_eq!(harness.view.pane(Side::Left).caret().line, 2);
}

#[test]
fn text_typed_into_a_focused_field_never_reaches_a_pane() {
    let mut harness = Harness::new(LEFT, RIGHT);
    harness.focus = Some(egui::Id::new("a-path-field"));
    harness.type_text("C:/some/path.txt");
    assert_eq!(harness.text(Side::Left), LEFT);
    assert_eq!(harness.text(Side::Right), RIGHT);
}

#[test]
fn typing_in_a_read_only_view_does_not_start_a_comparison() {
    let mut harness = Harness::read_only(LEFT, RIGHT);
    assert!(harness.view.is_settled());
    harness.click(harness.row_point(100.0, 0));
    harness.type_text("Z");
    assert_eq!(harness.text(Side::Left), LEFT);
    assert_eq!(harness.text(Side::Right), RIGHT);
    assert!(harness.view.is_settled());
}
