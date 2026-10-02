//! The Open With entries reach the menu and start a program.
//!
//! Nothing here starts a real process: the window is given a spawner that
//! records what it was handed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_app::cli::Startup;
use ca_app::App;
use ca_session::options::{LaunchCommand, OpenWithEntry};
use ca_session::SessionKind;
use ca_ui::launch::Spawner;
use ca_ui::testing::{context, raw_input};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a recording spawner was asked to start.
type Started = Arc<Mutex<Vec<LaunchCommand>>>;

/// A spawner that records what it was asked to start.
struct Recorder {
    started: Started,
}

impl Spawner for Recorder {
    fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
        self.started.lock().unwrap().push(command.clone());
        Ok(())
    }
}

fn app_in(dir: &Path, startup: Startup) -> App {
    let settings = dir.join("settings");
    let journals = dir.join("journals");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::create_dir_all(&journals).unwrap();
    App::from_startup_in(startup, &context(), settings, journals)
}

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

fn entry() -> OpenWithEntry {
    OpenWithEntry {
        description: "Editor".to_owned(),
        program: PathBuf::from("editor").into(),
        arguments: vec!["%f1".to_owned(), "%f2".to_owned()],
        ..OpenWithEntry::default()
    }
}

/// A text comparison over two files, with one stored entry, ready to run.
fn ready_app(dir: &Path) -> (App, Started) {
    let left = dir.join("left.txt");
    let right = dir.join("right.txt");
    std::fs::write(&left, b"alpha\n").unwrap();
    std::fs::write(&right, b"beta\n").unwrap();
    let mut app = app_in(dir, Startup::open(SessionKind::TextCompare, &left, &right));
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    assert!(run_frames(&mut app, App::active_is_ready));

    let started: Started = Started::default();
    app.set_spawner(Arc::new(Recorder {
        started: Arc::clone(&started),
    }));
    let mut options = app.options().options_or_default();
    options.open_with.entries.push(entry());
    app.apply_options(options);
    // One frame resolves the stored document into what the menu reads.
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    (app, started)
}

#[test]
fn a_file_comparison_lists_the_stored_entries_that_accept_files() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _started) = ready_app(dir.path());
    let listed: Vec<String> = app
        .open_with_entries()
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(listed, vec!["Editor".to_owned()]);
}

/// A folder-only entry is not offered over a file comparison.
#[test]
fn an_entry_that_accepts_only_folders_is_left_out_of_a_file_comparison() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _started) = ready_app(dir.path());
    let mut options = app.options().options_or_default();
    options.open_with.entries[0].accepts_files = false;
    options.open_with.entries[0].accepts_folders = true;
    app.apply_options(options);
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert!(app.open_with_entries().is_empty());
}

/// Starting an entry hands the spawner a program and separate arguments, with
/// each side substituted from the view.
#[test]
fn starting_an_entry_names_both_sides_as_separate_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, started) = ready_app(dir.path());
    app.start_open_with(0, &context());
    assert!(run_frames(&mut app, |_| !started
        .lock()
        .unwrap()
        .is_empty()));

    let started = started.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].program, PathBuf::from("editor"));
    assert_eq!(started[0].arguments.len(), 2);
    assert!(started[0].arguments[0].ends_with("left.txt"));
    assert!(started[0].arguments[1].ends_with("right.txt"));
}

/// The launcher tab names no file, so it offers no entry.
#[test]
fn a_view_that_names_no_file_offers_no_entry() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path(), Startup::Home);
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    let mut options = app.options().options_or_default();
    options.open_with.entries.push(entry());
    app.apply_options(options);
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert!(app.open_with_entries().is_empty());
}
