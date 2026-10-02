//! The update check reaches the window: at start, from the Help menu, and
//! through the notice to the browser.
//!
//! The release document is served by an in-process server, and the browser is
//! a spawner that records what it was handed. Nothing reaches the internet.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_app::cli::Startup;
use ca_app::shell::UpdateNotice;
use ca_app::{App, VERSION};
use ca_session::options::LaunchCommand;
use ca_ui::command::Command;
use ca_ui::launch::Spawner;
use ca_ui::testing::{context, raw_input};
use ca_vfs::testing::http_server::{HttpTestServer, Reply};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Recorder {
    started: Arc<Mutex<Vec<LaunchCommand>>>,
}

impl Spawner for Recorder {
    fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
        self.started.lock().unwrap().push(command.clone());
        Ok(())
    }
}

/// The tag of the build after this one.
fn next_tag() -> String {
    let mut parts: Vec<u32> = VERSION
        .split('.')
        .map(|part| part.parse().unwrap())
        .collect();
    parts[3] += 1;
    format!(
        "v{}.{:02}.{:02}.{:04}",
        parts[0], parts[1], parts[2], parts[3]
    )
}

fn server(tag: String) -> (HttpTestServer, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    let server = HttpTestServer::start(
        move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Reply::body(
                200,
                "application/json",
                format!(r#"{{"tag_name":"{tag}"}}"#).into_bytes(),
            )
        },
        None,
    );
    (server, asked)
}

fn app_in(dir: &Path, url: String) -> App {
    let settings = dir.join("settings");
    let journals = dir.join("journals");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::create_dir_all(&journals).unwrap();
    let mut app = App::from_startup_in(Startup::Home, &context(), settings, journals);
    app.check_updates_at(url);
    app
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

#[test]
fn a_newer_release_found_at_start_opens_its_page_in_the_browser() {
    let dir = tempfile::tempdir().unwrap();
    let (server, asked) = server(next_tag());
    let mut app = app_in(dir.path(), format!("{}/releases/latest", server.url()));
    let started = Arc::new(Mutex::new(Vec::new()));
    app.set_spawner(Arc::new(Recorder {
        started: Arc::clone(&started),
    }));
    assert!(run_frames(&mut app, |app| app.update_notice().is_some()));
    let Some(UpdateNotice::Newer(release)) = app.update_notice().cloned() else {
        panic!("{:?}", app.update_notice());
    };
    assert!(release.page.ends_with(&next_tag()));
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    app.open_release_page(&context().notify);
    assert!(app.update_notice().is_none());
    assert!(run_frames(&mut app, |_| !started
        .lock()
        .unwrap()
        .is_empty()));
    let command = started.lock().unwrap()[0].clone();
    assert_eq!(
        command.arguments.last().map(String::as_str),
        Some(release.page.as_str())
    );

    // A second window inside the interval asks nothing.
    let mut again = app_in(dir.path(), format!("{}/releases/latest", server.url()));
    assert!(run_frames(&mut again, |app| app.options().is_ready()));
    assert!(run_frames(&mut again, |app| !app.is_checking_for_updates()));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(again.update_notice().is_none());
}

#[test]
fn the_help_menu_check_says_when_no_newer_release_exists() {
    let dir = tempfile::tempdir().unwrap();
    let (server, asked) = server(format!("v{VERSION}"));
    let mut app = app_in(dir.path(), format!("{}/releases/latest", server.url()));
    assert!(run_frames(&mut app, |app| app.options().is_ready()
        && !app.is_checking_for_updates()));
    assert!(app.update_notice().is_none());
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(app.accepts(Command::CheckForUpdates));
    app.run(Command::CheckForUpdates, &context());
    assert!(run_frames(&mut app, |app| app.update_notice().is_some()));
    assert!(matches!(
        app.update_notice(),
        Some(UpdateNotice::Message(_))
    ));
    assert_eq!(asked.load(Ordering::SeqCst), 2);
}
