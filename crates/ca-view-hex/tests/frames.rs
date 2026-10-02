//! Real frames over two small files: what the view shows, where the caret
//! goes, and what typing and saving do to the content.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_ui::command::Command;
use ca_ui::save::CloseChoice;
use ca_ui::testing::{context, event_input, raw_input, sized_input};
use ca_ui::view::SessionView;
use ca_view_hex::find::PatternKind;
use ca_view_hex::model::{DisplayFilter, Side};
use ca_view_hex::{Area, HexView};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LEFT: [u8; 48] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
];

fn right_bytes() -> Vec<u8> {
    let mut bytes = LEFT.to_vec();
    if let Some(slot) = bytes.get_mut(20) {
        *slot = 0xFF;
    }
    bytes
}

struct Harness {
    view: HexView,
    ctx: egui::Context,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    left: PathBuf,
    right: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.bin");
        let right = dir.path().join("right.bin");
        std::fs::write(&left, LEFT).unwrap();
        std::fs::write(&right, right_bytes()).unwrap();
        let view = HexView::new(left.clone(), right.clone(), &context(), 1);
        Self {
            view,
            ctx: egui::Context::default(),
            dir,
            left,
            right,
        }
    }

    fn frame(&mut self, input: egui::RawInput) -> egui::FullOutput {
        self.view.tick();
        let context = context();
        let view = &mut self.view;
        self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        })
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame(raw_input());
        }
    }

    fn wait_ready(&mut self) {
        assert!(
            self.wait_until(|view| view.model().row_count() > 0),
            "the comparison never finished"
        );
        // The byte pane registers egui's arrow-key focus lock after its first
        // focused frame. Let that frame pass before sending keyboard input.
        self.frames(1);
    }

    fn press(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(event_input(
            1_280.0,
            800.0,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
        ));
    }

    fn type_text(&mut self, text: &str) {
        self.frame(event_input(
            1_280.0,
            800.0,
            vec![egui::Event::Text(text.to_owned())],
        ));
    }

    fn wait_until(&mut self, mut ready: impl FnMut(&HexView) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.frames(1);
            if ready(&self.view) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

#[test]
fn two_files_open_and_the_difference_is_laid_out() {
    let mut harness = Harness::new();
    harness.wait_ready();
    let model = harness.view.model();
    assert_eq!(model.counts().changed_bytes, 1);
    assert_eq!(model.counts().sections, 1);
    assert_eq!(model.side_len(Side::Left), 48);
    // Opening lands on the first difference rather than at the top.
    assert!(harness.view.caret_row() > 0);
}

#[test]
fn navigation_steps_through_the_differences_and_back() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.run(Command::NextSection);
    harness.frames(1);
    let at_section = harness.view.caret_row();
    harness.view.run(Command::PreviousSection);
    harness.frames(1);
    harness.view.run(Command::NextDifference);
    harness.frames(1);
    assert!(harness.view.caret_row() >= at_section.min(1));
    assert!(harness.view.accepts(Command::NextDifference));
}

#[test]
fn every_display_filter_paints_a_frame() {
    let mut harness = Harness::new();
    harness.wait_ready();
    let total = harness.view.model().row_count();
    for (command, filter) in [
        (Command::ShowDifferences, DisplayFilter::Differences),
        (Command::ShowSame, DisplayFilter::Same),
        (Command::ShowContext, DisplayFilter::Context(2)),
        (Command::ShowAll, DisplayFilter::All),
    ] {
        harness.view.run(command);
        harness.frames(1);
        assert_eq!(
            std::mem::discriminant(&harness.view.filter()),
            std::mem::discriminant(&filter)
        );
    }
    assert_eq!(harness.view.model().row_count(), total);
}

#[test]
fn changing_the_bytes_a_row_shows_relays_the_comparison() {
    let mut harness = Harness::new();
    harness.wait_ready();
    // A window this narrow leaves room for fewer bytes than a wide one, and the
    // layout has to follow without losing the caret.
    harness.view.place_caret(Side::Left, 20);
    harness.frame(sized_input(1_280.0, 800.0));
    let wide = harness.view.bytes_per_row();
    let wide_rows = harness.view.model().row_count();
    harness.frame(sized_input(700.0, 800.0));
    harness.frame(sized_input(700.0, 800.0));
    let narrow = harness.view.bytes_per_row();
    assert!(narrow < wide, "{narrow} is not fewer than {wide}");
    assert!(harness.view.model().row_count() > wide_rows);
    assert_eq!(harness.view.caret(Side::Left).offset, 20);
}

#[test]
fn the_caret_moves_between_the_two_areas_and_keeps_its_selection() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 4);
    harness.press(egui::Key::ArrowRight, egui::Modifiers::SHIFT);
    harness.press(egui::Key::ArrowRight, egui::Modifiers::SHIFT);
    let selected = harness.view.caret(Side::Left).selection_len();
    assert_eq!(selected, 2);
    assert_eq!(harness.view.caret(Side::Left).area, Area::Hex);
    harness.press(egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(harness.view.caret(Side::Left).area, Area::Chars);
    assert_eq!(harness.view.caret(Side::Left).selection_len(), selected);
}

#[test]
fn arrow_keys_move_the_caret_by_a_byte_and_by_a_row() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.press(egui::Key::ArrowRight, egui::Modifiers::NONE);
    assert_eq!(harness.view.caret(Side::Left).offset, 1);
    let width = u64::from(harness.view.bytes_per_row());
    harness.press(egui::Key::ArrowDown, egui::Modifiers::NONE);
    assert_eq!(harness.view.caret(Side::Left).offset, 1 + width);
    harness.press(egui::Key::ArrowUp, egui::Modifiers::NONE);
    assert_eq!(harness.view.caret(Side::Left).offset, 1);
    harness.press(egui::Key::ArrowLeft, egui::Modifiers::NONE);
    assert_eq!(harness.view.caret(Side::Left).offset, 0);
    // The caret never walks off the front of the file.
    harness.press(egui::Key::ArrowLeft, egui::Modifiers::NONE);
    assert_eq!(harness.view.caret(Side::Left).offset, 0);
}

#[test]
fn two_hex_digits_replace_the_byte_under_the_caret() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 3);
    harness.view.set_area(Area::Hex);
    harness.type_text("a");
    harness.type_text("b");
    assert_eq!(harness.view.buffer(Side::Left).byte(3), Some(0xAB));
    assert_eq!(harness.view.caret(Side::Left).offset, 4);
    assert!(harness.view.buffer(Side::Left).is_modified());
    assert!(harness.view.title().starts_with("* "));
}

#[test]
fn a_character_typed_in_the_character_area_writes_its_byte() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 5);
    harness.view.set_area(Area::Chars);
    harness.type_text("Z");
    assert_eq!(harness.view.buffer(Side::Left).byte(5), Some(b'Z'));
}

/// A text field that holds the keyboard takes what is typed, and the bytes
/// stay as they are.
#[test]
fn typing_into_the_go_to_field_leaves_the_bytes_alone() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 3);
    harness.view.set_area(Area::Hex);
    harness.view.run(Command::GoTo);
    harness.frames(2);
    assert!(
        harness.ctx.memory(egui::Memory::focused).is_some(),
        "the go to field did not take the keyboard"
    );
    harness.type_text("1");
    harness.press(egui::Key::Backspace, egui::Modifiers::NONE);
    harness.type_text("2");
    harness.frames(1);
    assert_eq!(harness.view.buffer(Side::Left).byte(3), Some(0x03));
    assert_eq!(harness.view.buffer(Side::Left).len(), 48);
    assert!(!harness.view.buffer(Side::Left).is_modified());
    assert!(!harness.view.accepts(Command::Undo));
}

/// Once the field lets the keyboard go, typing reaches the bytes again.
#[test]
fn the_bytes_take_the_keyboard_back_when_the_field_lets_it_go() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.run(Command::GoTo);
    harness.frames(2);
    harness.press(egui::Key::Escape, egui::Modifiers::NONE);
    harness.frames(2);
    harness.view.place_caret(Side::Left, 3);
    harness.view.set_area(Area::Hex);
    harness.type_text("a");
    harness.type_text("b");
    assert_eq!(harness.view.buffer(Side::Left).byte(3), Some(0xAB));
}

#[test]
fn inserting_grows_the_file_and_undo_puts_it_back() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.run(Command::ToggleOverwrite);
    harness.view.place_caret(Side::Left, 2);
    harness.view.set_area(Area::Chars);
    harness.type_text("Q");
    assert_eq!(harness.view.buffer(Side::Left).len(), 49);
    assert_eq!(harness.view.buffer(Side::Left).byte(2), Some(b'Q'));
    harness.view.run(Command::Undo);
    harness.frames(1);
    assert_eq!(harness.view.buffer(Side::Left).len(), 48);
    assert!(!harness.view.buffer(Side::Left).is_modified());
    harness.view.run(Command::Redo);
    harness.frames(1);
    assert_eq!(harness.view.buffer(Side::Left).len(), 49);
}

#[test]
fn deleting_takes_the_selection_out() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    for _ in 0..4 {
        harness.press(egui::Key::ArrowRight, egui::Modifiers::SHIFT);
    }
    assert_eq!(harness.view.caret(Side::Left).selection_len(), 4);
    harness.press(egui::Key::Delete, egui::Modifiers::NONE);
    assert_eq!(harness.view.buffer(Side::Left).len(), 44);
    assert_eq!(harness.view.buffer(Side::Left).byte(0), Some(0x04));
}

#[test]
fn a_change_is_compared_again_without_reading_the_files() {
    let mut harness = Harness::new();
    harness.wait_ready();
    let before = harness.view.model().counts().changed_bytes;
    harness.view.place_caret(Side::Left, 20);
    harness.view.set_area(Area::Hex);
    harness.type_text("f");
    harness.type_text("f");
    // The comparison behind the change runs on a worker after a quiet period.
    assert!(
        harness.wait_until(|view| view.model().counts().changed_bytes != before),
        "the comparison never followed the change"
    );
    assert_eq!(harness.view.model().counts().changed_bytes, 0);
    // Nothing was read back, so the change is still unwritten.
    assert!(harness.view.buffer(Side::Left).is_modified());
}

/// Emptying both sides is a change like any other: after the comparison that
/// follows, the change is still marked and Undo still reaches it.
#[test]
fn emptying_both_sides_keeps_the_change_and_its_undo() {
    let mut harness = Harness::new();
    harness.wait_ready();
    for side in [Side::Left, Side::Right] {
        harness.view.place_caret(side, 0);
        harness.view.run(Command::SelectAll);
        harness.view.run(Command::Cut);
        harness.frames(1);
    }
    assert_eq!(harness.view.buffer(Side::Left).len(), 0);
    assert_eq!(harness.view.buffer(Side::Right).len(), 0);
    assert!(
        harness.wait_until(|view| view.is_ready()
            && view.model().side_len(Side::Left) == 0
            && view.model().side_len(Side::Right) == 0),
        "the comparison never followed the change"
    );
    assert!(harness.view.buffer(Side::Left).is_modified());
    assert!(harness.view.buffer(Side::Right).is_modified());
    assert!(harness.view.accepts(Command::Undo));
    assert!(harness.view.title().starts_with("* "));
}

#[test]
fn a_search_runs_on_a_worker_and_moves_the_caret() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_find_pattern("2A 2B", PatternKind::Bytes);
    harness.view.run(Command::FindNext);
    assert!(harness.wait_until(|view| view.caret(Side::Left).offset == 42));
}

#[test]
fn a_text_search_finds_the_characters_the_area_shows() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("text-left.bin");
    let right = dir.path().join("text-right.bin");
    std::fs::write(&left, b"header NEEDLE trailer").unwrap();
    std::fs::write(&right, b"header needle trailer").unwrap();
    let mut harness = Harness {
        view: HexView::new(left.clone(), right.clone(), &context(), 2),
        ctx: egui::Context::default(),
        dir,
        left,
        right,
    };
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_find_pattern("NEEDLE", PatternKind::Text);
    harness.view.run(Command::FindNext);
    assert!(harness.wait_until(|view| view.caret(Side::Left).offset == 7));
}

#[test]
fn a_search_that_finds_nothing_says_so() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness
        .view
        .set_find_pattern("DE AD BE EF", PatternKind::Bytes);
    harness.view.run(Command::FindNext);
    assert!(harness.wait_until(|view| view.message() == Some("Not found.")));
}

#[test]
fn a_malformed_search_phrase_is_refused_before_a_worker_starts() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.set_find_pattern("zz", PatternKind::Bytes);
    harness.view.run(Command::FindNext);
    assert_eq!(
        harness.view.message(),
        Some("Enter pairs of hexadecimal digits.")
    );
}

#[test]
fn a_byte_address_reached_through_go_to_moves_both_the_caret_and_the_row() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Right, 0);
    harness.view.place_caret(Side::Right, 0x2F);
    harness.frames(1);
    assert_eq!(harness.view.caret(Side::Right).offset, 0x2F);
    assert_eq!(harness.view.active_side(), Side::Right);
    let row = harness
        .view
        .model()
        .row_of_offset(Side::Right, 0x2F)
        .unwrap();
    assert_eq!(harness.view.caret_row(), row);
}

#[test]
fn copying_to_the_other_side_replaces_the_bytes_there() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 20);
    harness.view.run(Command::CopyToRight);
    harness.frames(1);
    assert_eq!(harness.view.buffer(Side::Right).byte(20), Some(0x14));
    assert!(harness.view.buffer(Side::Right).is_modified());
}

#[test]
fn a_saved_change_reaches_the_file_and_leaves_no_temporary_behind() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_area(Area::Hex);
    harness.type_text("9");
    harness.type_text("9");
    assert!(harness.view.buffer(Side::Left).is_modified());
    harness.view.run(Command::SaveFile);
    assert!(harness.wait_until(|view| !view.buffer(Side::Left).is_modified()));
    let written = std::fs::read(&harness.left).unwrap();
    assert_eq!(written.first(), Some(&0x99));
    assert_eq!(written.len(), 48);
    let leftovers: Vec<_> = std::fs::read_dir(harness.left.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("saving"))
        .collect();
    assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    assert_eq!(std::fs::read(&harness.right).unwrap(), right_bytes());
}

#[test]
fn a_file_that_changed_on_disk_is_not_overwritten_without_an_answer() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_area(Area::Hex);
    harness.type_text("7");
    harness.type_text("7");
    // Something else writes the file while the change is held in the view.
    std::fs::write(&harness.left, [1u8, 2, 3]).unwrap();
    harness.view.run(Command::SaveFile);
    harness.frames(20);
    assert_eq!(std::fs::read(&harness.left).unwrap(), vec![1, 2, 3]);
    assert!(harness.view.buffer(Side::Left).is_modified());
    // The question is on screen, so the tab is still waiting on an answer.
    assert!(!harness.view.may_close());
}

#[test]
fn a_tab_with_unwritten_changes_refuses_to_close_until_it_has_asked() {
    let mut harness = Harness::new();
    harness.wait_ready();
    assert!(harness.view.may_close());
    harness.view.place_caret(Side::Left, 1);
    harness.view.set_area(Area::Hex);
    harness.type_text("1");
    harness.type_text("2");
    assert!(!harness.view.may_close());
}

#[test]
fn a_missing_file_reaches_a_terminal_state_and_reports_why() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = HexView::new(
        dir.path().join("absent-a.bin"),
        dir.path().join("absent-b.bin"),
        &context(),
        3,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !view.is_ready() {
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(view.is_ready());
    assert!(view.notice().is_some());
}

/// A pane with no bytes of its own on a row still has to paint, because that is
/// what an insertion on the other side leaves behind.
#[test]
fn a_gap_row_paints_without_reaching_for_bytes_that_are_not_there() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("short.bin");
    let right = dir.path().join("long.bin");
    std::fs::write(&left, vec![5u8; 64]).unwrap();
    let mut longer = vec![5u8; 32];
    longer.extend(std::iter::repeat_n(9u8, 64));
    longer.extend(std::iter::repeat_n(5u8, 32));
    std::fs::write(&right, &longer).unwrap();
    let mut harness = Harness {
        view: HexView::new(left.clone(), right.clone(), &context(), 4),
        ctx: egui::Context::default(),
        dir,
        left,
        right,
    };
    harness.wait_ready();
    harness.frames(4);
    let model = harness.view.model();
    let gaps = (0..model.row_count())
        .filter_map(|index| model.row(index))
        .filter(|row| row.left.is_none() || row.right.is_none())
        .count();
    assert!(gaps > 0, "the insertion produced no gap rows");
}

#[test]
fn reload_reads_the_files_again_from_disk() {
    let mut harness = Harness::new();
    harness.wait_ready();
    assert_eq!(harness.view.model().counts().changed_bytes, 1);
    // Something else rewrites the left file while the tab is open, making it
    // identical to the right file.
    std::fs::write(&harness.left, right_bytes()).unwrap();
    harness.view.run(Command::Reload);
    assert!(
        harness.wait_until(|view| view.buffer(Side::Left).byte(20) == Some(0xFF)),
        "the reload never picked up the change on disk"
    );
    assert_eq!(harness.view.model().counts().changed_bytes, 0);
}

/// Reload reads both files again, so a change that is not written raises the
/// question a close raises, and the change stays until it is answered.
#[test]
fn reload_with_a_change_asks_before_it_reads_the_files_again() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 3);
    harness.view.set_area(Area::Hex);
    harness.type_text("a");
    harness.type_text("b");
    assert!(harness.view.buffer(Side::Left).is_modified());
    harness.view.run(Command::Reload);
    harness.frames(1);
    assert_eq!(
        harness.view.prompt_text().as_deref(),
        Some("This tab has changes that are not written.")
    );
    assert_eq!(harness.view.buffer(Side::Left).byte(3), Some(0xAB));
    assert!(harness.view.accepts(Command::Undo));

    harness.view.answer_unsaved(CloseChoice::Discard);
    assert!(
        harness.wait_until(|view| view.is_ready() && view.buffer(Side::Left).byte(3) == Some(0x03)),
        "the reload never ran"
    );
    assert!(!harness.view.buffer(Side::Left).is_modified());
    assert_eq!(std::fs::read(&harness.left).unwrap(), LEFT);
}

/// Swap Sides reads both files again as well, so it asks the same question.
/// Saving first writes the change, then the sides change places.
#[test]
fn swap_sides_with_a_change_saves_first_when_asked_to() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_area(Area::Hex);
    harness.type_text("9");
    harness.type_text("9");
    let title = harness.view.title();
    harness.view.run(Command::SwapSides);
    harness.frames(1);
    assert_eq!(
        harness.view.prompt_text().as_deref(),
        Some("This tab has changes that are not written.")
    );
    assert_eq!(
        harness.view.title(),
        title,
        "the sides moved before the answer"
    );

    harness.view.answer_prompt(true);
    assert!(
        harness.wait_until(|view| view.is_ready()
            && !view.is_saving()
            && view.title() == "right.bin - left.bin"),
        "the swap never ran"
    );
    assert_eq!(std::fs::read(&harness.left).unwrap().first(), Some(&0x99));
    assert_eq!(harness.view.buffer(Side::Right).byte(0), Some(0x99));
    assert_eq!(harness.view.buffer(Side::Left).byte(0), Some(0x00));
    assert!(!harness.view.buffer(Side::Right).is_modified());
}

#[test]
fn hiding_the_address_gutter_frees_room_for_more_bytes_per_row() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.frame(sized_input(700.0, 800.0));
    harness.frame(sized_input(700.0, 800.0));
    let with_addresses = harness.view.bytes_per_row();
    harness.view.set_show_addresses(false);
    harness.frame(sized_input(700.0, 800.0));
    harness.frame(sized_input(700.0, 800.0));
    let without_addresses = harness.view.bytes_per_row();
    assert!(
        without_addresses > with_addresses,
        "{without_addresses} is not more than {with_addresses}"
    );
}

fn file_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("pair-left.bin");
    let right = dir.join("pair-right.bin");
    std::fs::write(&left, LEFT).unwrap();
    std::fs::write(&right, right_bytes()).unwrap();
    (left, right)
}

#[test]
fn every_command_the_view_declares_has_a_label_and_a_state() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = file_pair(dir.path());
    let mut view = HexView::new(left, right, &context(), 5);
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && view.model().row_count() == 0 {
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    let commands = view.commands();
    assert!(!commands.is_empty());
    for state in &commands {
        assert!(!state.command.label().is_empty());
        assert_eq!(state.enabled, view.accepts(state.command));
    }
}

#[test]
fn replace_swaps_the_found_bytes_and_moves_to_the_next_match() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("replace-left.bin");
    let right = dir.path().join("replace-right.bin");
    std::fs::write(&left, b"ab-ab-ab").unwrap();
    std::fs::write(&right, b"ab-ab-ab").unwrap();
    let mut harness = Harness {
        view: HexView::new(left.clone(), right.clone(), &context(), 3),
        ctx: egui::Context::default(),
        dir,
        left,
        right,
    };
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_find_pattern("ab", PatternKind::Text);
    harness.view.set_replacement("XYZ");
    harness.view.run(Command::FindNext);
    assert!(harness.wait_until(|view| view.caret(Side::Left).offset == 3));
    harness.view.replace_match();
    assert_eq!(harness.view.buffer(Side::Left).bytes(), b"ab-XYZ-ab");
    assert!(harness.wait_until(|view| view.caret(Side::Left).offset == 7));
    assert_eq!(harness.view.caret(Side::Left).selection(), Some(7..9));
}

#[test]
fn replace_all_is_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("all-left.bin");
    let right = dir.path().join("all-right.bin");
    std::fs::write(&left, [0x01, 0x02, 0x01, 0x02, 0x03]).unwrap();
    std::fs::write(&right, [0x01]).unwrap();
    let mut harness = Harness {
        view: HexView::new(left.clone(), right.clone(), &context(), 4),
        ctx: egui::Context::default(),
        dir,
        left,
        right,
    };
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_find_pattern("01 02", PatternKind::Bytes);
    harness.view.set_replacement("");
    harness.view.replace_all_matches();
    assert!(harness.wait_until(|view| view.buffer(Side::Left).bytes() == [0x03]));
    assert_eq!(harness.view.message(), Some("2 replaced."));
    harness.view.run(Command::Undo);
    assert_eq!(
        harness.view.buffer(Side::Left).bytes(),
        [0x01, 0x02, 0x01, 0x02, 0x03]
    );
    assert!(!harness.view.buffer(Side::Left).is_modified());
}

#[test]
fn the_replace_command_is_declared_and_opens_the_panel() {
    let mut harness = Harness::new();
    harness.wait_ready();
    assert!(harness.view.accepts(Command::Replace));
    harness.view.run(Command::Replace);
    harness.frames(1);
}

/// Find and Replace open their strip with the search field holding the
/// keyboard, so a phrase typed next goes into the field and the bytes stay as
/// they are.
#[test]
fn typing_after_find_or_replace_goes_into_the_search_field() {
    for command in [Command::Find, Command::Replace] {
        let mut harness = Harness::new();
        harness.wait_ready();
        harness.frames(3);
        harness.view.place_caret(Side::Left, 3);
        harness.view.set_area(Area::Hex);
        harness.view.set_find_pattern("", PatternKind::Bytes);
        harness.view.run(command);
        harness.frames(2);
        harness.type_text("a");
        harness.type_text("b");
        harness.frames(1);
        assert_eq!(
            harness.view.buffer(Side::Left).byte(3),
            Some(0x03),
            "{command:?}"
        );
        assert!(
            !harness.view.buffer(Side::Left).is_modified(),
            "{command:?}"
        );
        assert_eq!(harness.view.find_pattern(), "ab", "{command:?}");
    }
}

/// A save in flight keeps the tab from closing without a question, and the
/// tab says it is busy until the save ends.
#[test]
fn a_save_in_flight_keeps_the_tab_busy() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_area(Area::Hex);
    harness.type_text("9");
    harness.type_text("9");
    assert!(!SessionView::is_busy(&harness.view));
    harness.view.run(Command::SaveFile);
    assert!(harness.view.is_saving());
    assert!(SessionView::is_busy(&harness.view));
    assert!(!harness.view.may_close());
    assert!(harness.wait_until(|view| !view.is_saving()));
    assert!(!SessionView::is_busy(&harness.view));
}

/// With the editing switch of the Specs page on, typing reaches no byte, an
/// undo takes back no written change, and no save is offered. With the
/// switch off again, typing lands and the view reports the unwritten edit.
#[test]
fn the_editing_switch_turns_every_edit_and_save_off() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 5);
    harness.view.set_area(Area::Chars);
    harness.type_text("Z");
    harness.view.run(Command::SaveFile);
    assert!(harness.wait_until(|view| !view.is_saving() && !view.buffer(Side::Left).is_modified()));
    let mut settings = harness.view.session_settings();
    settings.specs.disable_editing = true;
    harness.view.apply_session_settings(&settings);
    harness.frames(1);
    for command in [
        Command::Undo,
        Command::Paste,
        Command::CopyToRight,
        Command::SaveFileAs,
    ] {
        assert!(!harness.view.accepts(command), "{command:?}");
    }
    harness.view.run(Command::Undo);
    harness.view.place_caret(Side::Left, 7);
    harness.type_text("Y");
    assert_eq!(harness.view.buffer(Side::Left).byte(5), Some(b'Z'));
    assert_eq!(harness.view.buffer(Side::Left).byte(7), Some(0x07));
    assert!(!harness.view.holds_unwritten_edits());

    settings.specs.disable_editing = false;
    harness.view.apply_session_settings(&settings);
    harness.frames(1);
    harness.view.place_caret(Side::Left, 7);
    harness.type_text("Y");
    assert_eq!(harness.view.buffer(Side::Left).byte(7), Some(b'Y'));
    assert!(harness.view.holds_unwritten_edits());
}

#[test]
fn edit_menu_paste_requests_the_clipboard_and_inserts_what_arrives() {
    let mut harness = Harness::new();
    harness.wait_ready();
    harness.view.place_caret(Side::Left, 0);
    harness.view.set_area(Area::Hex);
    harness.view.run(Command::Paste);
    let output = harness.frame(raw_input());
    let asked = output.viewport_output.values().any(|viewport| {
        viewport
            .commands
            .contains(&egui::ViewportCommand::RequestPaste)
    });
    assert!(asked);
    harness.frame(event_input(
        1_280.0,
        800.0,
        vec![egui::Event::Paste("A5".to_owned())],
    ));
    assert_eq!(harness.view.buffer(Side::Left).byte(0), Some(0xA5));
}
