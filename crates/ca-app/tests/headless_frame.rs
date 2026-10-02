//! Runs real frames of each view against an egui context with no window and no
//! graphics device, which is enough to catch a panic in layout or painting.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_app::cli::Startup;
use ca_app::App;
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::testing::{context, raw_input};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Run frames until `ready` is satisfied or the budget runs out, and report
/// whether it was satisfied.
fn run_frames(app: &mut App, ready: impl Fn(&App) -> bool) -> bool {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
        if ready(app) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    std::fs::create_dir_all(left.join("sub")).unwrap();
    std::fs::create_dir_all(right.join("sub")).unwrap();
    std::fs::write(left.join("same.txt"), b"alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(right.join("same.txt"), b"alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(left.join("sub/differs.txt"), b"one\ntwo\nthree\n").unwrap();
    std::fs::write(right.join("sub/differs.txt"), b"one\ntwo!\nthree\nfour\n").unwrap();
    std::fs::write(left.join("orphan.txt"), b"only here\n").unwrap();
    dir
}

fn side(dir: &Path, side: &str) -> PathBuf {
    dir.join(side)
}

/// A window built from `startup`, over a settings directory of its own.
///
/// Tests of one binary run in parallel threads and the test binaries run one
/// after another over the same folder. A shared settings directory therefore
/// lets one test's claim on it, or one test's stored document, decide what
/// another test sees. The directory is removed when the returned value is
/// dropped, so it is bound beside the window that reads it.
fn started_app(startup: Startup) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let app = App::from_startup_in(
        startup,
        &context(),
        dir.path().to_path_buf(),
        dir.path().join("journals"),
    );
    (dir, app)
}

#[test]
fn the_launcher_paints_a_frame() {
    let (_settings, mut app) = started_app(Startup::Home);
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert_eq!(app.tab_count(), 1);
    assert_eq!(app.active_title().as_deref(), Some("Home"));
}

#[test]
fn a_text_comparison_paints_frames_until_it_is_ready() {
    let dir = fixture();
    let (_settings, mut app) = started_app(Startup::open(
        SessionKind::TextCompare,
        &side(dir.path(), "left").join("sub").join("differs.txt"),
        &side(dir.path(), "right").join("sub").join("differs.txt"),
    ));
    let done = run_frames(&mut app, |app| {
        app.active_is_ready() && app.active_title().as_deref() == Some("differs.txt - differs.txt")
    });
    assert!(done);
    // A few more frames once the result has landed, so the rows, the thumbnail
    // and the status bar all paint against real data.
    let ctx = egui::Context::default();
    for _ in 0..5 {
        let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    }
}

#[test]
fn exit_keeps_a_text_edit_open() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, b"original\n").unwrap();
    std::fs::write(&right, b"original\n").unwrap();
    let (_settings, mut app) = started_app(Startup::open(SessionKind::TextCompare, &left, &right));
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !app.active_is_ready() && Instant::now() < deadline {
        let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.active_is_ready());
    let mut input = raw_input();
    input.events.push(egui::Event::Text("X".to_owned()));
    let _ = ctx.run(input, |ctx| app.frame(ctx));
    app.run(Command::Exit, &context());
    assert!(!app.is_closing());
    assert_eq!(app.tab_count(), 1);
    assert_eq!(std::fs::read(&left).unwrap(), b"original\n");
}

#[test]
fn a_folder_comparison_paints_frames_until_it_is_ready() {
    let dir = fixture();
    let (_settings, mut app) = started_app(Startup::open(
        SessionKind::FolderCompare,
        &side(dir.path(), "left"),
        &side(dir.path(), "right"),
    ));
    let done = run_frames(&mut app, |app| {
        app.active_is_ready() && app.active_title().as_deref() == Some("left - right")
    });
    assert!(done);
    let ctx = egui::Context::default();
    for _ in 0..10 {
        let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    }
}

#[test]
fn a_missing_file_still_paints() {
    let dir = tempfile::tempdir().unwrap();
    let (_settings, mut app) = started_app(Startup::open(
        SessionKind::TextCompare,
        &dir.path().join("absent-a.txt"),
        &dir.path().join("absent-b.txt"),
    ));
    let ctx = egui::Context::default();
    for _ in 0..20 {
        let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.tab_count(), 1);
}

#[test]
fn closing_the_last_tab_closes_the_window() {
    let (_settings, mut app) = started_app(Startup::Home);
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    app.close_tab(0);
    assert_eq!(app.tab_count(), 0);
    assert!(app.is_closing());
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
}
