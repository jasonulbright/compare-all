//! Behaviour every background job in this view has to keep: it reaches a
//! terminal state the view can see, a newer run supersedes an older one, and
//! every run asks for the repaint that shows its result.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_ui::command::Command;
use ca_ui::testing::{context, counting_context};
use ca_ui::view::SessionView;
use ca_view_hex::find::PatternKind;
use ca_view_hex::model::Side;
use ca_view_hex::HexView;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn tick_until(view: &mut HexView, ready: impl Fn(&HexView) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        view.tick();
        if ready(view) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Two files large enough that a comparison of them is still running when the
/// next line of the test cancels it.
fn slow_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let size = 24 * 1024 * 1024;
    let mut left: Vec<u8> = Vec::with_capacity(size);
    let mut seed = 0x1234_5678u32;
    for _ in 0..size {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        left.push(u8::try_from(seed >> 24).unwrap_or(0));
    }
    let mut right = Vec::with_capacity(size + size / 4_096);
    for (index, byte) in left.iter().enumerate() {
        if index % 4_096 == 0 {
            right.push(0xEE);
        }
        right.push(*byte);
    }
    let left_path = dir.join("slow-left.bin");
    let right_path = dir.join("slow-right.bin");
    std::fs::write(&left_path, &left).unwrap();
    std::fs::write(&right_path, &right).unwrap();
    (left_path, right_path)
}

fn small_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("small-left.bin");
    let right = dir.join("small-right.bin");
    std::fs::write(&left, [1u8, 2, 3, 4]).unwrap();
    std::fs::write(&right, [1u8, 2, 9, 4]).unwrap();
    (left, right)
}

#[test]
fn a_cancelled_comparison_reaches_a_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = slow_pair(dir.path());
    let mut view = HexView::new(left, right, &context(), 1);
    view.tick();
    view.run(Command::Cancel);
    // Without a terminal message on every exit path the view would stay in its
    // running state for as long as the tab is open.
    assert!(
        tick_until(&mut view, SessionView::is_ready),
        "the view never left its running state"
    );
}

#[test]
fn a_comparison_of_a_missing_file_reaches_a_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = HexView::new(
        dir.path().join("absent-a.bin"),
        dir.path().join("absent-b.bin"),
        &context(),
        1,
    );
    assert!(tick_until(&mut view, SessionView::is_ready));
}

/// A second run started while the first is going has to be the one whose result
/// lands, however the two finish against each other.
#[test]
fn a_newer_comparison_supersedes_the_one_it_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (slow_left, slow_right) = slow_pair(dir.path());
    let (small_left, small_right) = small_pair(dir.path());
    let mut view = HexView::new(slow_left, slow_right, &context(), 1);
    view.tick();
    // The paths change under the running comparison, which starts a new one.
    let mut replacement = HexView::new(small_left, small_right.clone(), &context(), 1);
    std::mem::swap(&mut view, &mut replacement);
    replacement.on_close();
    assert!(tick_until(&mut view, |view| view.model().row_count() > 0));
    assert_eq!(view.model().side_len(Side::Left), 4);
    assert_eq!(view.model().counts().changed_bytes, 1);
}

/// Swapping sides starts the comparison again. The result that lands has to be
/// the one for the sides as they now stand.
#[test]
fn swapping_sides_installs_the_result_of_the_run_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("wide.bin");
    let right = dir.path().join("narrow.bin");
    std::fs::write(&left, vec![3u8; 4_096]).unwrap();
    std::fs::write(&right, vec![3u8; 1_024]).unwrap();
    let mut view = HexView::new(left, right, &context(), 1);
    assert!(tick_until(&mut view, |view| view.model().row_count() > 0));
    assert_eq!(view.model().side_len(Side::Left), 4_096);
    view.run(Command::SwapSides);
    assert!(tick_until(&mut view, |view| view
        .model()
        .side_len(Side::Left)
        == 1_024));
    assert_eq!(view.model().side_len(Side::Right), 4_096);
}

#[test]
fn a_view_asks_for_a_repaint_however_its_work_was_started() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = small_pair(dir.path());
    let wakes = Arc::new(AtomicUsize::new(0));
    let context = counting_context(Arc::clone(&wakes));
    let mut view = HexView::new(left, right, &context, 1);
    assert!(tick_until(&mut view, |view| view.model().row_count() > 0));
    let after_first = wakes.load(Ordering::SeqCst);
    assert!(after_first > 0);
    view.run(Command::Recompare);
    assert!(tick_until(&mut view, SessionView::is_ready));
    assert!(
        wakes.load(Ordering::SeqCst) > after_first,
        "the run started by recomparing asked for no repaint"
    );
}

#[test]
fn a_cancelled_search_reaches_a_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = slow_pair(dir.path());
    let mut view = HexView::new(left, right, &context(), 1);
    assert!(tick_until(&mut view, |view| view.model().row_count() > 0));
    view.place_caret(Side::Left, 0);
    view.set_find_pattern("DE AD BE EF CA FE BA BE", PatternKind::Bytes);
    view.run(Command::FindNext);
    view.run(Command::Cancel);
    assert!(tick_until(&mut view, |view| !view.accepts(Command::Cancel)));
}

/// A search started while another is running replaces it, so only one answer
/// ever reaches the caret.
#[test]
fn a_newer_search_replaces_the_one_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("hay.bin");
    let right = dir.path().join("hay-right.bin");
    let mut bytes = vec![0u8; 1 << 20];
    if let Some(slot) = bytes.get_mut(900_000) {
        *slot = 0xAA;
    }
    if let Some(slot) = bytes.get_mut(950_000) {
        *slot = 0xBB;
    }
    std::fs::write(&left, &bytes).unwrap();
    std::fs::write(&right, &bytes).unwrap();
    let mut view = HexView::new(left, right, &context(), 1);
    assert!(tick_until(&mut view, |view| view.model().row_count() > 0));
    view.place_caret(Side::Left, 0);
    view.set_find_pattern("AA", PatternKind::Bytes);
    view.run(Command::FindNext);
    view.set_find_pattern("BB", PatternKind::Bytes);
    view.run(Command::FindNext);
    assert!(tick_until(&mut view, |view| view.caret(Side::Left).offset == 950_000));
    // The first search is gone, so its answer never arrives afterwards.
    for _ in 0..200 {
        view.tick();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(view.caret(Side::Left).offset, 950_000);
}

#[test]
fn closing_a_tab_stops_everything_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = slow_pair(dir.path());
    let mut view = HexView::new(left, right, &context(), 1);
    view.tick();
    view.on_close();
    assert!(tick_until(&mut view, SessionView::is_ready));
}

#[test]
fn a_read_only_request_refuses_every_save_and_edit() {
    use ca_ui::view::ViewFactory;
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.bin");
    let right = dir.path().join("right.bin");
    std::fs::write(&left, [1_u8, 2, 3]).unwrap();
    std::fs::write(&right, [1_u8, 9, 3]).unwrap();
    let request = ca_ui::view::OpenRequest::new(ca_session::SessionKind::HexCompare, left, right)
        .over_temporaries(Vec::new());
    let mut view = HexView::create_from(&request, &context(), 1);
    assert!(tick_until(&mut view, SessionView::is_ready));
    for command in [
        Command::SaveFile,
        Command::SaveFileAs,
        Command::SaveBoth,
        Command::CopyToLeft,
        Command::CopyToRight,
    ] {
        assert!(!view.accepts(command), "{command:?} is accepted");
    }
}
