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

/// The reasons of the ten row commands, from the view and from the toolbar.
fn row_reasons(view: &TextView) -> Vec<(String, Option<&'static str>)> {
    use ca_ui::toolbar::Item;

    let mut found: Vec<(String, Option<&'static str>)> = [
        Command::CopyToRight,
        Command::CopyLineToRight,
        Command::CopyToLeft,
        Command::CopyLineToLeft,
        Command::CopyToOtherSide,
        Command::SelectSection,
        Command::NextSection,
        Command::PreviousSection,
        Command::NextDifference,
        Command::PreviousDifference,
    ]
    .into_iter()
    .map(|command| (format!("{command:?}"), view.refusal(command)))
    .collect();
    for item in view.toolbar_items() {
        if let Item::Command {
            name,
            enabled,
            reason,
            ..
        } = item
        {
            if ["copy", "next-section", "previous-section"].contains(&name) {
                assert!(!enabled, "{name} is enabled");
                found.push((name.to_owned(), Some(reason)));
            }
        }
    }
    found
}

fn assert_every_row_reason(view: &TextView, expected: &str) {
    for (name, reason) in row_reasons(view) {
        assert_eq!(reason, Some(expected), "{name}");
    }
}

#[test]
fn row_commands_say_the_text_is_compared_again_after_a_rule_change_with_no_edit() {
    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    let mut rules = fixture.view.rules();
    rules.case_unimportant = !rules.case_unimportant;
    fixture.view.set_rules(rules);
    assert_every_row_reason(&fixture.view, super::NOT_COMPARED_AGAIN);
    fixture.view.run(Command::NextSection);
    assert_eq!(
        fixture.view.message(),
        Some(super::WAIT_FOR_COMPARISON_AGAIN)
    );

    fixture.view.left_pane.place(Caret::new(0, 0), false);
    fixture.view.left_pane.type_character('z');
    fixture.view.note_edit();
    assert_every_row_reason(&fixture.view, super::EDIT_NOT_COMPARED);
    fixture.view.run(Command::NextSection);
    assert_eq!(fixture.view.message(), Some(super::WAIT_FOR_COMPARISON));

    fixture.settle();
    let mut settings = fixture.view.session_settings().clone();
    settings.alignment.never_align_differences = !settings.alignment.never_align_differences;
    fixture.view.apply_session_settings(settings);
    assert!(!fixture.view.rows_current());
    assert_every_row_reason(&fixture.view, super::NOT_COMPARED_AGAIN);
}

#[test]
fn an_edit_after_a_rule_change_is_named_as_the_wait() {
    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    fixture.view.left_pane.place(Caret::new(0, 0), false);
    fixture.view.left_pane.type_character('z');
    fixture.view.note_edit();
    let mut rules = fixture.view.rules();
    rules.case_unimportant = !rules.case_unimportant;
    fixture.view.set_rules(rules);
    assert_every_row_reason(&fixture.view, super::EDIT_NOT_COMPARED);
}

#[test]
fn a_failed_or_stopped_comparison_names_reload_as_the_way_out() {
    use super::Status;

    for (status, locked, expected) in [
        (
            Status::Failed("unreadable".to_owned()),
            false,
            super::FAILED,
        ),
        (Status::Cancelled, false, super::STOPPED),
        (
            Status::Failed("unreadable".to_owned()),
            true,
            super::FAILED_LOCKED,
        ),
        (Status::Cancelled, true, super::STOPPED_LOCKED),
    ] {
        let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
        fixture.view.status = status.clone();
        fixture.view.locked = locked;
        let mut reasons = row_reasons(&fixture.view);
        reasons.retain(|(name, _)| !locked || !(name.contains("Copy") || name == "copy"));
        for (name, reason) in reasons {
            assert_eq!(reason, Some(expected), "{status:?} {name}");
        }
        let report = fixture
            .view
            .toolbar_items()
            .into_iter()
            .find_map(|item| match item {
                ca_ui::toolbar::Item::Command {
                    name: "report",
                    reason,
                    ..
                } => Some(reason),
                _ => None,
            });
        assert_eq!(report, Some(expected), "{status:?} report");
        fixture.view.run(Command::NextSection);
        assert_eq!(
            fixture.view.message(),
            Some(format!("{expected}.").as_str()),
            "{status:?}"
        );
    }
}

/// No command the view accepts carries a reason for refusing it.
fn assert_no_reason_for_an_accepted_command(view: &TextView, state: &str) {
    for command in Command::ALL {
        let reason = view.refusal(*command);
        assert!(
            reason.is_none() || !view.accepts(*command),
            "{state}: {command:?} is accepted and refused with {reason:?}"
        );
    }
}

#[test]
fn a_copy_into_a_read_only_pane_names_that_pane() {
    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    fixture.view.right_pane.set_read_only(true);
    assert_no_reason_for_an_accepted_command(&fixture.view, "settled");
    for command in [Command::CopyToRight, Command::CopyLineToRight] {
        assert!(!fixture.view.accepts(command));
        assert_eq!(
            fixture.view.refusal(command),
            Some(super::RIGHT_READ_ONLY),
            "{command:?}"
        );
    }
    assert_eq!(fixture.view.active, Side::Left);
    assert_eq!(
        fixture.view.refusal(Command::CopyToOtherSide),
        Some(super::RIGHT_READ_ONLY)
    );
    assert!(fixture.view.accepts(Command::CopyToLeft));
    assert_eq!(fixture.view.refusal(Command::CopyToLeft), None);

    fixture.view.left_pane.place(Caret::new(0, 0), false);
    fixture.view.left_pane.type_character('z');
    fixture.view.note_edit();
    assert_no_reason_for_an_accepted_command(&fixture.view, "after a left edit");
    assert_eq!(
        fixture.view.refusal(Command::CopyToRight),
        Some(super::RIGHT_READ_ONLY)
    );
    assert_eq!(
        fixture.view.refusal(Command::CopyToLeft),
        Some(super::EDIT_NOT_COMPARED)
    );

    fixture.view.active = Side::Right;
    fixture.view.right_pane.set_read_only(false);
    fixture.view.left_pane.set_read_only(true);
    assert_eq!(
        fixture.view.refusal(Command::CopyToOtherSide),
        Some(super::LEFT_READ_ONLY)
    );
    assert_eq!(
        fixture.view.refusal(Command::CopyLineToLeft),
        Some(super::LEFT_READ_ONLY)
    );
}

#[test]
fn a_copy_with_editing_off_names_the_read_only_pane_and_a_fixed_view_says_it_is_read_only() {
    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    let mut settings = fixture.view.session_settings().clone();
    settings.specs.disable_editing = true;
    fixture.view.apply_session_settings(settings);
    fixture.settle();
    assert_no_reason_for_an_accepted_command(&fixture.view, "editing off");
    assert_eq!(
        fixture.view.refusal(Command::CopyToRight),
        Some(super::RIGHT_READ_ONLY)
    );
    assert_eq!(
        fixture.view.refusal(Command::CopyToLeft),
        Some(super::LEFT_READ_ONLY)
    );

    fixture.view.locked = true;
    assert_no_reason_for_an_accepted_command(&fixture.view, "locked");
    for command in [
        Command::CopyToRight,
        Command::CopyLineToLeft,
        Command::CopyToOtherSide,
    ] {
        assert_eq!(
            fixture.view.refusal(command),
            Some(super::LOCKED),
            "{command:?}"
        );
    }
}

/// The reason a disabled row control gives is the one drawn while the pointer
/// rests on it and the one its accessibility node describes.
#[test]
fn a_row_control_shows_the_wait_for_a_comparison_of_unchanged_text_on_hover() {
    use ca_ui::testing::probe::{painted_texts, Probe};

    let mut fixture = Fixture::new(COPY_LEFT, COPY_RIGHT);
    let mut rules = fixture.view.rules();
    rules.case_unimportant = !rules.case_unimportant;
    fixture.view.set_rules(rules);
    let context = ca_ui::testing::context();
    let mut probe = Probe::new(2_400.0, 900.0);
    let view = &mut fixture.view;
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            view.ui(ui, &context);
        });
    };
    probe.idle(&mut draw);
    probe.idle(&mut draw);
    for label in ["Next Section", "Copy to Right"] {
        let control = probe.find_all(label).remove(0);
        assert!(!control.enabled, "{label}");
        let output = probe.hover(control.rect.center(), &mut draw);
        let painted = painted_texts(&output);
        assert!(
            painted.iter().any(|text| text == super::NOT_COMPARED_AGAIN),
            "{label}: painted {painted:?}"
        );
        let described = probe
            .find_all(label)
            .into_iter()
            .find(|found| found.rect == control.rect)
            .and_then(|found| found.description);
        assert_eq!(
            described.as_deref(),
            Some(super::NOT_COMPARED_AGAIN),
            "{label}"
        );
    }
}

/// Sections at rows 1, 3..5 and 7; rows 3 and 4 hold no left line.
const GAP_LEFT: &str = "a\nb\nc\nd\ne\nf\n";
const GAP_RIGHT: &str = "a\nB\nc\nX\nY\nd\ne\nF\n";

#[test]
fn navigation_moves_past_a_section_the_active_pane_has_no_line_in() {
    let mut fixture = Fixture::new(GAP_LEFT, GAP_RIGHT);
    assert_eq!(fixture.view.active, Side::Left);
    assert_eq!(fixture.view.current_row(), 1);
    let mut starts = Vec::new();
    for _ in 0..3 {
        fixture.view.run(Command::NextSection);
        fixture.frame(Vec::new());
        starts.push(fixture.view.current_section_rows().map(|rows| rows.start));
    }
    assert_eq!(starts, [Some(3), Some(7), Some(1)]);
    let mut rows = Vec::new();
    for _ in 0..4 {
        fixture.view.run(Command::NextDifference);
        fixture.frame(Vec::new());
        rows.push(fixture.view.current_row());
    }
    assert_eq!(rows, [3, 4, 7, 1]);
    let mut back = Vec::new();
    for _ in 0..3 {
        fixture.view.run(Command::PreviousSection);
        fixture.frame(Vec::new());
        back.push(fixture.view.current_section_rows().map(|rows| rows.start));
    }
    assert_eq!(back, [Some(7), Some(3), Some(1)]);
}

#[test]
fn a_caret_move_after_navigation_makes_the_caret_row_current_again() {
    let mut fixture = Fixture::new(GAP_LEFT, GAP_RIGHT);
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_row(), 3);
    fixture.frame(vec![egui::Event::Key {
        key: egui::Key::ArrowDown,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }]);
    assert_eq!(fixture.view.pane(Side::Left).caret().line, 3);
    assert_eq!(fixture.view.current_row(), 5);
}

/// Rows 0 and 1 hold no left line; the second section is row 4.
const TOP_LEFT: &str = "c\nd\ne\n";
const TOP_RIGHT: &str = "a\nb\nc\nd\nE\n";

#[test]
fn a_section_at_the_top_with_no_line_on_the_active_side_is_made_current() {
    let mut fixture = Fixture::new(TOP_LEFT, TOP_RIGHT);
    assert_eq!(
        fixture.view.current_section_rows(),
        Some(0..2),
        "the load did not make the first difference current"
    );
    fixture.view.left_pane.place(Caret::new(2, 0), false);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(4..5));
    fixture.view.run(Command::PreviousSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(0..2));
    fixture.view.run(Command::CopyToLeft);
    assert_eq!(fixture.text(Side::Left), "a\nb\nc\nd\ne\n");
    fixture.undo_all(Side::Left);
    assert_eq!(fixture.text(Side::Left), TOP_LEFT);

    fixture.settle();
    fixture.view.left_pane.place(Caret::new(2, 0), false);
    fixture.frame(Vec::new());
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(
        fixture.view.current_section_rows(),
        Some(0..2),
        "a wrap to the first section made another section current"
    );
}

/// Compare again with no edit: change one importance rule, let that
/// comparison land, then change the rule back and let it land.
fn recompare_without_an_edit(fixture: &mut Fixture) {
    for _ in 0..2 {
        let mut rules = fixture.view.rules();
        rules.case_unimportant = !rules.case_unimportant;
        fixture.view.set_rules(rules);
        fixture.settle();
    }
}

#[test]
fn a_top_section_stays_current_through_a_comparison_with_no_edit() {
    let mut fixture = Fixture::new(TOP_LEFT, TOP_RIGHT);
    fixture.view.left_pane.place(Caret::new(2, 0), false);
    fixture.frame(Vec::new());
    fixture.view.run(Command::PreviousSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(0..2));
    recompare_without_an_edit(&mut fixture);
    assert_eq!(
        fixture.view.current_section_rows(),
        Some(0..2),
        "a comparison with no edit moved the current section"
    );
    fixture.view.run(Command::CopyToLeft);
    assert_eq!(fixture.text(Side::Left), "a\nb\nc\nd\ne\n");
}

#[test]
fn a_gap_section_under_show_differences_stays_current_through_a_comparison_with_no_edit() {
    let mut fixture = Fixture::new(GAP_LEFT, GAP_RIGHT);
    fixture.view.run(Command::ShowDifferences);
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(3..5));
    recompare_without_an_edit(&mut fixture);
    assert_eq!(
        fixture.view.current_section_rows(),
        Some(3..5),
        "a comparison with no edit moved the current section"
    );
    fixture.view.run(Command::CopyToLeft);
    assert_eq!(fixture.text(Side::Left), "a\nb\nc\nX\nY\nd\ne\nf\n");
}

#[test]
fn next_section_after_a_comparison_with_no_edit_moves_on_from_the_section_reached() {
    let mut fixture = Fixture::new(GAP_LEFT, GAP_RIGHT);
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(3..5));
    recompare_without_an_edit(&mut fixture);
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(
        fixture.view.current_section_rows().map(|rows| rows.start),
        Some(7)
    );
}

/// A rule change can turn the section a move reached into matching text. The
/// row of the line the move reached stays current, and the copy commands act
/// on the next section below it, as they do for a caret on a matching line.
#[test]
fn a_section_a_rule_change_removes_leaves_its_row_current_and_the_next_section_to_copy() {
    let mut fixture = Fixture::new("a\nb\nc\nd\ne\n", "a\nB\nc\nX\ne\n");
    let mut rules = fixture.view.rules();
    rules.case_unimportant = false;
    fixture.view.set_rules(rules);
    fixture.settle();
    fixture.view.set_ignore_unimportant(true);
    fixture.view.left_pane.place(Caret::new(0, 0), false);
    fixture.frame(Vec::new());
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(1..2));

    rules.case_unimportant = true;
    fixture.view.set_rules(rules);
    fixture.settle();
    assert_eq!(fixture.view.data.model.section_of(1), None);
    assert_eq!(fixture.view.current_row(), 1);
    assert_eq!(fixture.view.current_section_rows(), Some(3..4));
    fixture.view.run(Command::CopyToLeft);
    assert_eq!(fixture.text(Side::Left), "a\nb\nc\nX\ne\n");
}

/// 300 lines that differ on lines 20 and 150.
fn long_pair() -> (String, String) {
    let side = |differing: &str| -> String {
        (0..300)
            .map(|line| {
                if line == 20 || line == 150 {
                    format!("{differing} {line}\n")
                } else {
                    format!("line {line}\n")
                }
            })
            .collect()
    };
    (side("LEFT"), side("RIGHT"))
}

#[test]
fn navigation_under_show_same_leaves_the_caret_on_a_shown_line() {
    let (left, right) = long_pair();
    let mut fixture = Fixture::new(&left, &right);
    fixture.view.left_pane.place(Caret::new(50, 0), false);
    fixture.frame(Vec::new());
    fixture.view.run(Command::ShowSame);
    for command in [
        Command::NextSection,
        Command::PreviousSection,
        Command::NextDifference,
        Command::PreviousDifference,
    ] {
        fixture.view.run(command);
        assert_eq!(fixture.view.pane(Side::Left).caret(), Caret::new(50, 0));
        assert_eq!(fixture.view.message(), Some(super::FILTER_HIDES));
    }
    fixture.frame(vec![egui::Event::Text("Z".to_owned())]);
    assert_eq!(fixture.view.pane(Side::Left).line_text(50), "Zline 50");
    assert_eq!(fixture.view.pane(Side::Left).line_text(150), "LEFT 150");
}

#[test]
fn navigation_under_show_differences_leaves_the_caret_on_a_shown_line() {
    let mut fixture = Fixture::new(GAP_LEFT, GAP_RIGHT);
    fixture.view.run(Command::ShowDifferences);
    fixture.view.run(Command::NextSection);
    fixture.frame(Vec::new());
    assert_eq!(fixture.view.current_section_rows(), Some(3..5));
    for side in [Side::Left, Side::Right] {
        let line = fixture.view.pane(side).caret().line;
        let row = match side {
            Side::Left => fixture.view.data.model.row_of_left_line(line),
            Side::Right => fixture.view.data.model.row_of_right_line(line),
        };
        assert!(
            row.is_some_and(|row| fixture.view.visible.shows(row)),
            "the {side:?} caret is on line {line}, which the filter hides"
        );
    }
}

/// One single-line difference after each of `sections` matching lines.
fn alternating_pair(sections: usize) -> (String, String) {
    use std::fmt::Write as _;

    let mut left = String::new();
    let mut right = String::new();
    for index in 0..sections {
        let _ = write!(left, "same {index}\nL {index}\n");
        let _ = write!(right, "same {index}\nR {index}\n");
    }
    (left, right)
}

/// One move reads a number of sections that grows with the logarithm of the
/// section count, under every filter, from either end and from the middle,
/// whether or not it wraps.
#[test]
fn one_section_move_reads_few_sections_under_every_filter() {
    use super::DisplayFilter;

    const SECTIONS: usize = 1_000;
    let (left, right) = alternating_pair(SECTIONS);
    let mut fixture = Fixture::new(&left, &right);
    assert_eq!(fixture.view.data.model.sections().len(), SECTIONS);
    let rows = fixture.view.data.model.row_count();
    let bound = 2 * (SECTIONS.ilog2() as usize + 2);
    for filter in [
        DisplayFilter::All,
        DisplayFilter::Differences,
        DisplayFilter::Same,
        DisplayFilter::Context(2),
        DisplayFilter::None,
    ] {
        fixture.view.set_filter(filter);
        for wrap in [true, false] {
            fixture.view.navigation.wrap_around = wrap;
            for command in [
                Command::NextSection,
                Command::PreviousSection,
                Command::NextDifference,
                Command::PreviousDifference,
            ] {
                for start in [0, rows / 2, rows - 1] {
                    fixture.view.caret = start;
                    fixture.view.anchor = None;
                    let _ = fixture.view.data.model.take_section_reads();
                    fixture.view.run(command);
                    let reads = fixture.view.data.model.take_section_reads();
                    assert!(
                        reads <= bound,
                        "{command:?} from row {start} under {filter:?}, wrap {wrap}, \
                         read {reads} sections of {SECTIONS}"
                    );
                }
            }
        }
    }
}

#[test]
fn navigation_shows_the_start_of_the_line_it_moves_to() {
    let mut fixture = Fixture::new(GAP_LEFT, GAP_RIGHT);
    fixture.view.horizontal = 40.0;
    fixture.view.run(Command::NextSection);
    assert!(fixture.view.horizontal.abs() < f32::EPSILON);
}
