//! Commands that turn rows into lines: the copies, Select Section and the
//! difference navigation.
//!
//! A frame here runs `ui` alone. `tick` is what installs a finished comparison,
//! so between two frames without `tick` the comparison an edit started is held
//! and the row map stays the one of the text before the edit.

use super::{Side, TextView};
use ca_ui::command::Command;
use ca_ui::editor::Caret;
use ca_ui::view::SessionView;
use std::time::{Duration, Instant};

pub(crate) struct Fixture {
    pub(crate) view: TextView,
    pub(crate) ctx: egui::Context,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// A view over two files, loaded and settled.
    pub(crate) fn new(left: &str, right: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let left_path = dir.path().join("left.txt");
        let right_path = dir.path().join("right.txt");
        std::fs::write(&left_path, left).expect("the left file is written");
        std::fs::write(&right_path, right).expect("the right file is written");
        let view = TextView::new(left_path, right_path, &ca_ui::testing::context(), 1);
        let mut fixture = Self {
            view,
            ctx: egui::Context::default(),
            _dir: dir,
        };
        fixture.settle();
        fixture
    }

    /// One frame carrying `events`, with no `tick` before it.
    pub(crate) fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        let context = ca_ui::testing::context();
        let input = ca_ui::testing::event_input(1_000.0, 600.0, events);
        let view = &mut self.view;
        self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        })
    }

    /// Install every comparison the view is waiting for.
    ///
    /// A comparison an edit made due starts at once rather than after the
    /// quiet period, so the wait is the comparison itself.
    pub(crate) fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self.view.job.is_none() && self.view.rediff.is_stale() {
                self.view.start_rediff();
            }
            self.view.tick();
            self.frame(Vec::new());
            if self.view.is_settled() && self.view.rows_current() {
                return;
            }
            assert!(Instant::now() < deadline, "the comparison never settled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(crate) fn text(&self, side: Side) -> String {
        self.view.pane(side).buffer().text()
    }

    /// Undo every edit of one pane.
    pub(crate) fn undo_all(&mut self, side: Side) {
        self.view.active = side;
        while self.view.pane(side).buffer().can_undo() {
            self.view.run(Command::Undo);
        }
    }
}

const COPY_LEFT: &str = "a\nB1\nB2\nc\nD\ne\n";
const COPY_RIGHT: &str = "a\nx\nc\ny\ne\n";

#[test]
fn a_copy_issued_before_the_last_copy_is_compared_is_refused() {
    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    assert_eq!(fixture.view.current_section_rows(), Some(1..3));
    fixture.view.run(Command::CopyToRight);
    assert!(!fixture.view.accepts(Command::CopyToRight));
    assert!(!fixture.view.accepts(Command::CopyToOtherSide));
    fixture.view.run(Command::CopyToRight);
    assert_eq!(fixture.text(Side::Right), "a\nB1\nB2\nc\ny\ne\n");
    assert_eq!(fixture.view.message(), Some(super::WAIT_FOR_COMPARISON));

    fixture.settle();
    assert_eq!(
        fixture.view.current_section_rows(),
        Some(4..5),
        "the copy did not move on to the next difference once compared"
    );
    fixture.view.run(Command::CopyToRight);
    assert_eq!(fixture.text(Side::Right), COPY_LEFT);
    fixture.undo_all(Side::Right);
    assert_eq!(fixture.text(Side::Right), COPY_RIGHT);
    assert_eq!(fixture.text(Side::Left), COPY_LEFT);
}

#[test]
fn a_move_and_a_copy_issued_before_the_last_copy_is_compared_are_refused() {
    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    fixture.view.navigation.go_to_next_after_copy = false;
    fixture.view.run(Command::CopyToRight);
    for command in [
        Command::NextSection,
        Command::PreviousSection,
        Command::NextDifference,
        Command::PreviousDifference,
        Command::SelectSection,
        Command::CopyToLeft,
        Command::CopyLineToRight,
        Command::CopyLineToLeft,
    ] {
        assert!(!fixture.view.accepts(command), "{command:?}");
    }
    let carets = (
        fixture.view.pane(Side::Left).caret(),
        fixture.view.pane(Side::Right).caret(),
    );
    fixture.view.run(Command::NextSection);
    assert_eq!(
        (
            fixture.view.pane(Side::Left).caret(),
            fixture.view.pane(Side::Right).caret(),
        ),
        carets,
        "navigation moved the carets over a row map of the text before the copy"
    );
    fixture.view.run(Command::CopyToRight);
    assert_eq!(fixture.text(Side::Right), "a\nB1\nB2\nc\ny\ne\n");

    fixture.settle();
    fixture.view.navigation.go_to_next_after_copy = false;
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(4..5));
    fixture.view.run(Command::CopyToRight);
    assert_eq!(fixture.text(Side::Right), COPY_LEFT);
    fixture.undo_all(Side::Right);
    assert_eq!(fixture.text(Side::Right), COPY_RIGHT);
}

#[test]
fn typing_is_not_held_while_an_edit_waits_for_its_comparison() {
    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    fixture.view.run(Command::CopyToRight);
    fixture.frame(vec![egui::Event::Text("Z".to_owned())]);
    assert_eq!(fixture.text(Side::Right), "a\nZB1\nB2\nc\ny\ne\n");
}

#[test]
fn navigation_after_lines_are_inserted_waits_for_their_comparison() {
    let mut fixture = Fixture::new("a\nb\nc\nD\ne\n", "a\nb\nc\nX\ne\n");
    fixture.view.left_pane.place(Caret::new(0, 0), false);
    fixture.view.left_pane.insert(&"new\n".repeat(10));
    fixture.view.note_edit();
    let before = fixture.view.pane(Side::Left).caret();
    fixture.view.run(Command::NextSection);
    assert_eq!(fixture.view.pane(Side::Left).caret(), before);
    fixture.settle();
    fixture.view.left_pane.place(Caret::new(0, 0), false);
    fixture.frame(Vec::new());
    fixture.view.run(Command::NextSection);
    let line = fixture.view.pane(Side::Left).caret().line;
    assert_eq!(fixture.view.pane(Side::Left).line_text(line), "D");
}

#[test]
fn row_controls_say_why_they_wait_for_the_comparison_of_an_edit() {
    use ca_ui::testing::probe::Probe;
    use ca_ui::toolbar::Item;

    let mut fixture = Fixture::new(
        "one\nTWO\nthree\nfour\nfive\n",
        "one\ntwo\nthree\nfour\nfive\n",
    );
    let reasons = |view: &TextView| -> Vec<(&'static str, bool, &'static str)> {
        view.toolbar_items()
            .into_iter()
            .filter_map(|item| match item {
                Item::Command {
                    name,
                    enabled,
                    reason,
                    ..
                } if ["copy", "next-section", "previous-section"].contains(&name) => {
                    Some((name, enabled, reason))
                }
                _ => None,
            })
            .collect()
    };
    assert!(reasons(&fixture.view)
        .iter()
        .all(|(_, enabled, _)| *enabled));

    fixture.view.left_pane.place(Caret::new(4, 0), false);
    fixture.view.left_pane.type_character('z');
    fixture.view.note_edit();
    for (name, enabled, reason) in reasons(&fixture.view) {
        assert!(
            !enabled,
            "{name} stays enabled over a row map of older text"
        );
        assert_eq!(reason, super::EDIT_NOT_COMPARED, "{name}");
    }

    let context = ca_ui::testing::context();
    let mut probe = Probe::new(2_400.0, 900.0);
    {
        let view = &mut fixture.view;
        let mut draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        };
        probe.idle(&mut draw);
        probe.idle(&mut draw);
        for label in ["Copy", "Copy to Right", "Copy to Left", "Next Section"] {
            let control = probe.find(label).unwrap_or_else(|error| panic!("{error}"));
            assert!(!control.enabled, "{label} is enabled over a stale row map");
            probe.press_at(control.rect.center(), &mut draw);
        }
    }
    assert_eq!(fixture.text(Side::Right), "one\ntwo\nthree\nfour\nfive\n");
    assert_eq!(fixture.text(Side::Left), "one\nTWO\nthree\nfour\nzfive\n");

    fixture.settle();
    {
        let view = &mut fixture.view;
        let mut draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        };
        probe.idle(&mut draw);
        let arrows = probe.find_all("Copy to Right");
        assert!(!arrows.is_empty());
        assert!(arrows.iter().all(|arrow| arrow.enabled));
        probe.press_at(arrows[0].rect.center(), &mut draw);
    }
    assert_eq!(fixture.text(Side::Right), "one\nTWO\nthree\nfour\nfive\n");
}
