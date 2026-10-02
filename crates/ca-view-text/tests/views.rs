//! Behaviour the text view has to keep: every job reaches a terminal state the
//! view can see, and every run asks for the repaint that shows its result.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_ui::command::Command;
use ca_ui::testing::{context, counting_context};
use ca_ui::view::SessionView;
use ca_view_text::TextView;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Tick a view until `ready` holds, or give up.
fn tick_until(view: &mut TextView, ready: impl Fn(&TextView) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
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
    use std::fmt::Write;
    let mut left = String::new();
    let mut right = String::new();
    for index in 0..200_000 {
        let _ = writeln!(left, "left line {index} of the comparison");
        let _ = writeln!(right, "right line {index} of the comparison");
    }
    let left_path = dir.join("slow-left.txt");
    let right_path = dir.join("slow-right.txt");
    std::fs::write(&left_path, left).unwrap();
    std::fs::write(&right_path, right).unwrap();
    (left_path, right_path)
}

#[test]
fn a_cancelled_text_comparison_reaches_a_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = slow_pair(dir.path());
    let mut view = TextView::new(left, right, &context(), 1);
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
fn a_text_comparison_of_a_missing_file_reaches_a_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = TextView::new(
        dir.path().join("absent-a.txt"),
        dir.path().join("absent-b.txt"),
        &context(),
        1,
    );
    assert!(tick_until(&mut view, SessionView::is_ready));
}

#[test]
fn a_view_asks_for_a_repaint_however_its_work_was_started() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, b"alpha\nbeta\n").unwrap();
    std::fs::write(&right, b"alpha\nbeta\n").unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let context = counting_context(Arc::clone(&wakes));
    let mut view = TextView::new(left, right, &context, 1);
    assert!(tick_until(&mut view, SessionView::is_ready));
    let after_first = wakes.load(Ordering::SeqCst);
    assert!(after_first > 0);
    // Swapping sides starts the work again; the second run has to wake the
    // frame loop exactly as the first one did.
    view.run(Command::SwapSides);
    assert!(tick_until(&mut view, SessionView::is_ready));
    assert!(
        wakes.load(Ordering::SeqCst) > after_first,
        "the run started by swapping sides asked for no repaint"
    );
}

#[test]
fn a_read_only_request_refuses_every_save_and_edit() {
    use ca_ui::view::ViewFactory;
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, b"one\ntwo\n").unwrap();
    std::fs::write(&right, b"one\nTWO\n").unwrap();
    let request = ca_ui::view::OpenRequest::new(ca_session::SessionKind::TextCompare, left, right)
        .over_temporaries(Vec::new());
    let mut view = TextView::create_from(&request, &context(), 1);
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
