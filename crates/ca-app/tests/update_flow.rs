//! Updates in the window: no call when switched off, the warning at launch,
//! a staged update that a launch leaves alone, and the apply at a normal
//! close. A local server stands in for the release host.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_app::cli::Startup;
use ca_app::shell::UpdateNotice;
use ca_app::update::{finish_at_exit, ExitHook, Source};
use ca_app::{App, VERSION};
use ca_session::options::LaunchCommand;
use ca_session::{AdminPolicies, PolicyKey};
use ca_ui::command::Command;
use ca_ui::launch::Spawner;
use ca_ui::testing::probe::Probe;
use ca_ui::testing::{context, raw_input};
use ca_update::flow::Endpoints;
use ca_update::manifest::msi_name;
use ca_update::mode::InstallMode;
use ca_update::staging::{AssetKind, DownloadWarning, StagedRecord, Staging};
use ca_update::Version;
use ca_vfs::testing::http_server::{HttpTestServer, Reply};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Recorder(Arc<Mutex<Vec<LaunchCommand>>>);

impl Spawner for Recorder {
    fn spawn(&self, command: &LaunchCommand) -> Result<(), String> {
        self.0.lock().unwrap().push(command.clone());
        Ok(())
    }
}

fn next_version() -> Version {
    let mut parts: Vec<u32> = VERSION.split('.').map(|p| p.parse().unwrap()).collect();
    parts[3] += 1;
    Version::parse(&format!(
        "{}.{}.{}.{}",
        parts[0], parts[1], parts[2], parts[3]
    ))
    .unwrap()
}

/// A server that names `tag` as the newest release, answers every other path
/// with `other`, and counts requests.
fn release_server(tag: String, other: u16) -> (HttpTestServer, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    let server = HttpTestServer::start(
        move |request| {
            count.fetch_add(1, Ordering::SeqCst);
            if request.path.ends_with("/latest") {
                Reply::body(
                    200,
                    "application/json",
                    format!(r#"{{"tag_name":"v{tag}"}}"#).into_bytes(),
                )
            } else if request.path.ends_with("/update-manifest.json") {
                let manifest = ca_update::manifest::Manifest {
                    schema: ca_update::manifest::SCHEMA,
                    version: tag.clone(),
                    minimum_updater_version: 1,
                    assets: vec![ca_update::manifest::ManifestAsset {
                        name: msi_name(&tag),
                        size: 100,
                        sha256: "a".repeat(64),
                    }],
                };
                Reply::body(
                    200,
                    "application/json",
                    serde_json::to_vec(&manifest).unwrap(),
                )
            } else {
                Reply::body(other, "text/html", b"<html>blocked</html>".to_vec())
            }
        },
        None,
    );
    (server, asked)
}

fn source(server: &HttpTestServer, staging: &Path, mode: Option<InstallMode>) -> Source {
    let base = server.url();
    Source {
        endpoints: Endpoints {
            latest: format!("{base}/latest"),
            download_base: format!("{base}/download"),
            page_base: format!("{base}/tag"),
        },
        mode,
        staging: staging.to_path_buf(),
        allow_plain_http: true,
    }
}

fn app_in(dir: &Path) -> App {
    let settings = dir.join("settings");
    let journals = dir.join("journals");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::create_dir_all(&journals).unwrap();
    App::from_startup_in(Startup::Home, &context(), settings, journals)
}

/// Record a check just now, so the interval has not passed and the launch
/// asks the server nothing.
fn check_just_done(dir: &Path) {
    let settings = dir.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(settings.join("update-check"), format!("{seconds}\n")).unwrap();
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

fn quiet_frames(app: &mut App, count: u32) {
    let ctx = egui::Context::default();
    for _ in 0..count {
        let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stage_fake(staging: &Path, version: &Version) -> StagedRecord {
    let name = AssetKind::Msi.file_name(version);
    assert_eq!(name, msi_name(&version.to_string()));
    std::fs::create_dir_all(staging).unwrap();
    std::fs::write(staging.join(&name), b"staged bytes").unwrap();
    let record = StagedRecord::new(version, name, "c".repeat(64), 12);
    Staging::new(staging.to_path_buf())
        .write_record(&record)
        .unwrap();
    record
}

#[test]
fn no_call_is_made_when_the_option_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let (server, asked) = release_server(next_version().to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(&server, &dir.path().join("stage"), None));
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    let mut options = app.options().options_or_default();
    options.tweaks.check_for_updates = false;
    app.apply_options(options);
    quiet_frames(&mut app, 40);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(!app.is_checking_for_updates());
}

#[test]
fn no_call_is_made_when_the_policy_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let (server, asked) = release_server(next_version().to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(&server, &dir.path().join("stage"), None));
    let mut keys = BTreeMap::new();
    keys.insert(PolicyKey::DisableCheckForUpdates, true);
    app.set_policies(AdminPolicies::from_keys(&keys));
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    quiet_frames(&mut app, 40);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(!app.accepts(Command::CheckForUpdates));
}

#[test]
fn a_good_check_and_a_blocked_download_warn_at_each_launch_until_installed() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let version = next_version();
    let (server, _) = release_server(version.to_string(), 403);
    let mode = Some(InstallMode::Installed { machine: false });

    let mut first = app_in(dir.path());
    first.update_from(source(&server, &staging, mode));
    assert!(run_frames(&mut first, |app| matches!(
        app.update_notice(),
        Some(UpdateNotice::NotDownloaded(_))
    )));

    // The next launch warns again.
    let mut second = app_in(dir.path());
    second.update_from(source(&server, &staging, mode));
    let opened = Arc::new(Mutex::new(Vec::new()));
    second.set_spawner(Arc::new(Recorder(Arc::clone(&opened))));
    assert!(run_frames(&mut second, |app| matches!(
        app.update_notice(),
        Some(UpdateNotice::NotDownloaded(_))
    )));

    // Cancel closes the warning and opens nothing.
    let mut probe = Probe::new(1280.0, 800.0);
    {
        let mut run = |ctx: &egui::Context| second.frame(ctx);
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe.click("Cancel", &mut run).unwrap();
        probe.idle(&mut run);
    }
    assert!(second.update_notice().is_none());
    assert!(opened.lock().unwrap().is_empty());

    // OK opens the release page of that version.
    let mut third = app_in(dir.path());
    third.update_from(source(&server, &staging, mode));
    third.set_spawner(Arc::new(Recorder(Arc::clone(&opened))));
    assert!(run_frames(&mut third, |app| matches!(
        app.update_notice(),
        Some(UpdateNotice::NotDownloaded(_))
    )));
    {
        let mut run = |ctx: &egui::Context| third.frame(ctx);
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe.click("OK", &mut run).unwrap();
        probe.idle(&mut run);
    }
    assert!(run_frames(&mut third, |_| !opened
        .lock()
        .unwrap()
        .is_empty()));
    let command = opened.lock().unwrap()[0].clone();
    assert!(command
        .arguments
        .last()
        .unwrap()
        .ends_with(&format!("/tag/v{version}")));
}

#[test]
fn a_launch_with_a_verified_staged_update_starts_normally_and_does_not_apply_it() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let version = next_version();
    let record = stage_fake(&staging, &version);
    check_just_done(dir.path());
    let (server, _) = release_server(version.to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(
        &server,
        &staging,
        Some(InstallMode::Installed { machine: false }),
    ));
    let hook = app.update_exit_hook();

    assert!(run_frames(&mut app, |app| matches!(
        app.update_notice(),
        Some(UpdateNotice::Ready(_))
    )));
    quiet_frames(&mut app, 20);

    let staged = Staging::new(staging.clone());
    assert_eq!(staged.read_record(), Some(record.clone()));
    assert!(staging.join(&record.asset).exists());
    assert!(!staging.join("applying.lock").exists());
    assert!(!staging.join("helper").exists());
    assert!(!hook.normal_close.load(Ordering::SeqCst));
    assert!(!app.is_closing());
    assert!(app.tab_count() >= 1);
}

#[test]
fn the_restart_button_closes_through_the_normal_path_and_asks_for_a_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let version = next_version();
    stage_fake(&staging, &version);
    check_just_done(dir.path());
    let (server, _) = release_server(version.to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(
        &server,
        &staging,
        Some(InstallMode::Installed { machine: false }),
    ));
    let hook = app.update_exit_hook();
    assert!(run_frames(&mut app, |app| matches!(
        app.update_notice(),
        Some(UpdateNotice::Ready(_))
    )));
    let mut probe = Probe::new(1280.0, 800.0);
    {
        let mut run = |ctx: &egui::Context| app.frame(ctx);
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe.click("Restart to update", &mut run).unwrap();
        probe.idle(&mut run);
    }
    assert!(run_frames(&mut app, App::is_closing));
    assert!(hook.normal_close.load(Ordering::SeqCst));
    assert!(hook.relaunch.load(Ordering::SeqCst));
}

#[test]
fn a_cancelled_close_cancels_the_update() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    app.open_home(&context());
    assert!(app.tab_count() > 1);
    let hook = app.update_exit_hook();
    app.restart_to_update();
    if app.asks_before_closing() {
        app.answer_exit(false);
    }
    quiet_frames(&mut app, 3);
    assert!(!app.is_closing());
    assert!(!hook.normal_close.load(Ordering::SeqCst));
}

#[test]
fn the_apply_starts_only_after_a_normal_close_with_a_newer_staged_update() {
    let dir = tempfile::tempdir().unwrap();
    let staging_path = dir.path().join("stage");
    let staging = Staging::new(staging_path.clone());
    let started = Mutex::new(Vec::new());
    let start = |_: &Staging, relaunch: bool| {
        started.lock().unwrap().push(relaunch);
        Ok(())
    };

    // Nothing staged: nothing to apply.
    let closed = ExitHook::default();
    closed.normal_close.store(true, Ordering::SeqCst);
    assert!(!finish_at_exit(&closed, &staging, &start));

    stage_fake(&staging_path, &next_version());

    // A crash or a forced end never sets the flag.
    assert!(!finish_at_exit(&ExitHook::default(), &staging, &start));
    assert!(started.lock().unwrap().is_empty());

    // A normal close applies and does not start the program again.
    assert!(finish_at_exit(&closed, &staging, &start));
    assert_eq!(*started.lock().unwrap(), vec![false]);

    // The button asks for a restart.
    closed.relaunch.store(true, Ordering::SeqCst);
    assert!(finish_at_exit(&closed, &staging, &start));
    assert_eq!(*started.lock().unwrap(), vec![false, true]);

    // A staged build that is not newer is left alone.
    let old = Version::parse(VERSION).unwrap();
    stage_fake(&staging_path, &old);
    assert!(!finish_at_exit(&closed, &staging, &start));
}

#[test]
fn an_earlier_failure_is_dropped_once_the_version_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    let staging_path = dir.path().join("stage");
    let (server, _) = release_server(VERSION.to_owned(), 404);
    Staging::new(staging_path.clone())
        .write_warning(&DownloadWarning::new(
            &Version::parse(VERSION).unwrap(),
            format!("{}/tag/v{VERSION}", server.url()),
        ))
        .unwrap();
    let mut app = app_in(dir.path());
    app.update_from(source(&server, &staging_path, None));
    quiet_frames(&mut app, 30);
    assert!(app.update_notice().is_none());
    assert!(run_frames(&mut app, |app| !app.is_checking_for_updates()));
    assert!(Staging::new(staging_path).read_warning().is_none());
}
