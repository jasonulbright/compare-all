//! The Startup and Tabs pages reach the window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_app::cli::Startup;
use ca_app::App;
use ca_session::options::{NewSessionPlacement, ProgramOptions};
use ca_ui::command::Command;
use ca_ui::testing::{context, raw_input};
use std::path::Path;
use std::time::{Duration, Instant};

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

fn one_frame(app: &mut App) {
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
}

/// A window with the options read and the stated edit in force.
fn app_with(dir: &Path, edit: impl FnOnce(&mut ProgramOptions)) -> App {
    let mut app = app_in(dir, Startup::Home);
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    let mut options = app.options().options_or_default();
    edit(&mut options);
    app.apply_options(options);
    one_frame(&mut app);
    app
}

#[test]
fn the_window_stays_open_after_its_last_tab_when_the_page_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(dir.path(), |options| {
        options.tabs.close_window_with_last_tab = false;
    });
    assert_eq!(app.tab_count(), 1);
    app.run(Command::CloseTab, &context());
    assert_eq!(app.tab_count(), 0);
    assert!(!app.is_closing(), "the window closed with its last tab");

    let mut app = app_with(dir.path(), |options| {
        options.tabs.close_window_with_last_tab = true;
    });
    app.run(Command::CloseTab, &context());
    assert!(app.is_closing());
}

#[test]
fn closing_several_tabs_asks_first_when_the_page_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(dir.path(), |options| {
        options.tabs.confirm_closing_several_tabs = true;
    });
    app.run(Command::NewTab, &context());
    assert_eq!(app.tab_count(), 2);

    app.run(Command::Exit, &context());
    assert!(app.asks_before_closing());
    assert!(!app.is_closing());

    app.answer_exit(false);
    assert!(!app.asks_before_closing());
    assert!(!app.is_closing());

    app.run(Command::Exit, &context());
    app.answer_exit(true);
    assert!(app.is_closing());
}

#[test]
fn a_cleared_confirmation_closes_several_tabs_without_a_question() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(dir.path(), |options| {
        options.tabs.confirm_closing_several_tabs = false;
    });
    app.run(Command::NewTab, &context());
    app.run(Command::Exit, &context());
    assert!(!app.asks_before_closing());
    assert!(app.is_closing());
}

/// The tab strip is drawn for one tab only when the page asks for it.
#[test]
fn the_tab_strip_is_left_out_of_a_single_tab_window_when_the_page_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let shown = app_with(dir.path(), |options| {
        options.tabs.show_tab_strip_with_one_tab = true;
    });
    assert_eq!(shown.tab_count(), 1);
    assert!(shown.shows_tab_strip());

    let mut hidden = app_with(dir.path(), |options| {
        options.tabs.show_tab_strip_with_one_tab = false;
    });
    assert!(!hidden.shows_tab_strip());
    // A second tab brings the strip back whatever the page says.
    hidden.run(Command::NewTab, &context());
    one_frame(&mut hidden);
    assert!(hidden.shows_tab_strip());
}

/// A session started from the launcher takes the launcher's tab when the page
/// asks for it, rather than opening beside it.
#[test]
fn a_new_session_takes_the_launcher_tab_when_the_page_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(dir.path(), |options| {
        options.tabs.new_session_placement = NewSessionPlacement::from_id("reuseHomeTab");
    });
    assert_eq!(app.tab_count(), 1);
    app.from_launcher(&context(), |app, context| {
        app.open_kind(
            &ca_session::SessionKind::TextCompare,
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            context,
        );
    });
    assert_eq!(app.tab_count(), 1, "the launcher tab was kept");
    assert_ne!(app.active_title().as_deref(), Some("Home"));

    let mut beside = app_with(dir.path(), |options| {
        options.tabs.new_session_placement = NewSessionPlacement::from_id("newTab");
    });
    beside.from_launcher(&context(), |app, context| {
        app.open_kind(
            &ca_session::SessionKind::TextCompare,
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            context,
        );
    });
    assert_eq!(beside.tab_count(), 2);
}

/// The workspace the Startup page names is opened, not the last one used.
#[test]
fn the_startup_page_names_the_workspace_that_opens() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path(), Startup::Home);
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    assert!(run_frames(&mut app, |app| app
        .store()
        .try_borrow()
        .is_ok_and(|handle| handle.is_ready())));

    app.run(Command::NewTab, &context());
    assert_eq!(app.tab_count(), 2);
    app.apply_workspace(
        &ca_ui::workspace::WorkspaceAction::Save("Two".to_owned()),
        &context(),
    );

    let mut options = app.options().options_or_default();
    options.startup.load_workspace = "Two".to_owned();
    app.apply_options(options);
    // Both documents are written on a worker, so the next window sees them only
    // once those writes have reported.
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    assert!(run_frames(&mut app, |app| app
        .store()
        .try_borrow()
        .is_ok_and(|handle| handle.is_ready())));
    drop(app);

    let mut again = app_in(dir.path(), Startup::Home);
    assert!(run_frames(&mut again, |app| app.tab_count() == 2));
}
