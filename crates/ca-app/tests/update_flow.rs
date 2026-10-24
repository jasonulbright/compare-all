//! Updates in the window: no call when switched off, the warning at launch,
//! a staged update that a launch leaves alone, and the apply at a normal
//! close. A local server stands in for the release host.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_app::cli::Startup;
use ca_app::shell::UpdateNotice;
use ca_app::update::{finish_at_exit, ExitHook, Source};
use ca_app::{App, VERSION};
use ca_session::options::LaunchCommand;
use ca_session::{AdminPolicies, PolicyKey, PolicyLoader};
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
    assert!(run_frames(&mut app, App::launch_update_check_done));
    assert_eq!(asked.load(Ordering::SeqCst), 0);
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
fn a_policy_file_that_disables_checks_stops_the_call_and_hides_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join("compare-all.conf");
    std::fs::write(&conf, "DisableCheckForUpdates=yes\n").unwrap();
    let policies = ca_session::FilePolicySource::new(&conf).load().unwrap();
    let (server, asked) = release_server(next_version().to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(&server, &dir.path().join("stage"), None));
    app.set_policies(policies);
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    quiet_frames(&mut app, 40);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(!app.accepts(Command::CheckForUpdates));
}

#[test]
fn an_unreadable_policy_store_stops_the_call_and_shows_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let load = ca_session::load_policies(&ca_session::FilePolicySource::new(dir.path()));
    let (server, asked) = release_server(next_version().to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(&server, &dir.path().join("stage"), None));
    app.set_policies(load.policies);
    app.set_policy_warning(load.warning);
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    quiet_frames(&mut app, 40);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(!app.accepts(Command::CheckForUpdates));
    assert!(app
        .policy_notice()
        .is_some_and(|text| text.contains("cannot read policy store")));
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
    assert!(run_frames(&mut app, App::launch_update_check_done));
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
    closed.apply_allowed.store(true, Ordering::SeqCst);
    assert!(!finish_at_exit(&closed, &staging, &start));

    stage_fake(&staging_path, &next_version());

    // A crash or a forced end never sets the flag.
    let crashed = ExitHook::default();
    crashed.apply_allowed.store(true, Ordering::SeqCst);
    assert!(!finish_at_exit(&crashed, &staging, &start));
    assert!(started.lock().unwrap().is_empty());

    // Checks turned off, or an install for every user, never apply.
    let refused = ExitHook::default();
    refused.normal_close.store(true, Ordering::SeqCst);
    assert!(!finish_at_exit(&refused, &staging, &start));
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

fn write_warning(staging: &Path, server: &HttpTestServer, version: &Version) {
    Staging::new(staging.to_path_buf())
        .write_warning(&DownloadWarning::new(
            version,
            format!("{}/tag/v{version}", server.url()),
        ))
        .unwrap();
}

#[test]
fn a_cancelled_restart_does_not_relaunch_after_a_later_close_of_the_last_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    app.open_home(&context());
    assert!(app.tab_count() > 1);
    let hook = app.update_exit_hook();
    app.restart_to_update();
    assert!(app.asks_before_closing());
    app.answer_exit(false);
    while app.tab_count() > 0 {
        app.close_tab(0);
    }
    assert!(app.is_closing());
    quiet_frames(&mut app, 2);
    assert!(hook.normal_close.load(Ordering::SeqCst));
    assert!(!hook.relaunch.load(Ordering::SeqCst));
}

#[test]
fn a_download_warning_shows_without_a_network_call_when_the_option_is_off() {
    {
        let by_policy = false;
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("stage");
        let version = next_version();
        let (server, asked) = release_server(version.to_string(), 404);
        write_warning(&staging, &server, &version);
        let mut app = app_in(dir.path());
        app.update_from(source(
            &server,
            &staging,
            Some(InstallMode::Installed { machine: false }),
        ));
        if by_policy {
            let mut keys = BTreeMap::new();
            keys.insert(PolicyKey::DisableCheckForUpdates, true);
            app.set_policies(AdminPolicies::from_keys(&keys));
        }
        assert!(run_frames(&mut app, |app| app.options().is_ready()));
        let mut options = app.options().options_or_default();
        options.tweaks.check_for_updates = by_policy;
        app.apply_options(options);
        assert!(
            run_frames(&mut app, |app| matches!(
                app.update_notice(),
                Some(UpdateNotice::NotDownloaded(_))
            )),
            "policy {by_policy}"
        );
        quiet_frames(&mut app, 10);
        assert_eq!(asked.load(Ordering::SeqCst), 0, "policy {by_policy}");
    }
}

/// A window whose update checks are off by the option or by the policy, over
/// a verified staged update, once the launch has reported what it found.
fn launch_with_checks_off(
    dir: &Path,
    staging: &Path,
    server: &HttpTestServer,
    by_policy: bool,
) -> App {
    let mut app = app_in(dir);
    app.update_from(source(
        server,
        staging,
        Some(InstallMode::Installed { machine: false }),
    ));
    if by_policy {
        let mut keys = BTreeMap::new();
        keys.insert(PolicyKey::DisableCheckForUpdates, true);
        app.set_policies(AdminPolicies::from_keys(&keys));
    }
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    let mut options = app.options().options_or_default();
    options.tweaks.check_for_updates = by_policy;
    app.apply_options(options);
    app
}

#[test]
fn a_staged_update_with_the_option_off_is_offered_and_applied_only_by_the_button() {
    {
        let by_policy = false;
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("stage");
        let version = next_version();
        let record = stage_fake(&staging, &version);
        let (server, asked) = release_server(version.to_string(), 404);
        let started = Mutex::new(Vec::new());
        let start = |_: &Staging, relaunch: bool| {
            started.lock().unwrap().push(relaunch);
            Ok(())
        };

        // A plain close with checks off applies nothing.
        let mut app = launch_with_checks_off(dir.path(), &staging, &server, by_policy);
        let hook = app.update_exit_hook();
        assert!(
            run_frames(&mut app, |app| matches!(
                app.update_notice(),
                Some(UpdateNotice::Ready(_))
            )),
            "policy {by_policy}: {:?}",
            app.update_notice()
        );
        assert_eq!(
            Staging::new(staging.clone()).read_record(),
            Some(record.clone())
        );
        assert!(staging.join(&record.asset).exists());
        app.run(Command::Exit, &context());
        if app.asks_before_closing() {
            app.answer_exit(true);
        }
        assert!(run_frames(&mut app, App::is_closing));
        assert!(!finish_at_exit(
            &hook,
            &Staging::new(staging.clone()),
            &start
        ));
        assert!(started.lock().unwrap().is_empty());

        // The button applies it and starts the program again.
        let mut app = launch_with_checks_off(dir.path(), &staging, &server, by_policy);
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
        if app.asks_before_closing() {
            app.answer_exit(true);
        }
        assert!(run_frames(&mut app, App::is_closing));
        assert!(finish_at_exit(
            &hook,
            &Staging::new(staging.clone()),
            &start
        ));
        assert_eq!(*started.lock().unwrap(), vec![true]);
        assert_eq!(asked.load(Ordering::SeqCst), 0, "policy {by_policy}");
    }
}

#[test]
fn the_policy_shows_no_update_notice_and_applies_nothing_but_keeps_the_staged_files() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let version = next_version();
    let record = stage_fake(&staging, &version);
    let (server, asked) = release_server(version.to_string(), 404);
    write_warning(&staging, &server, &version);
    Staging::new(staging.clone()).write_apply_failure("cannot place ca.exe");
    let mut app = launch_with_checks_off(dir.path(), &staging, &server, true);
    let hook = app.update_exit_hook();
    assert!(run_frames(&mut app, App::launch_update_check_done));
    quiet_frames(&mut app, 30);
    assert!(!app.is_checking_for_updates());
    assert_eq!(app.update_notice(), None);
    app.run(Command::CheckForUpdates, &context());
    quiet_frames(&mut app, 10);
    assert_eq!(app.update_notice(), None);
    app.restart_to_update();
    if app.asks_before_closing() {
        app.answer_exit(true);
    }
    assert!(run_frames(&mut app, App::is_closing));
    let started = Mutex::new(0);
    let start = |_: &Staging, _: bool| {
        *started.lock().unwrap() += 1;
        Ok(())
    };
    assert!(!finish_at_exit(
        &hook,
        &Staging::new(staging.clone()),
        &start
    ));
    assert_eq!(*started.lock().unwrap(), 0);
    let kept = Staging::new(staging.clone());
    assert_eq!(kept.read_record(), Some(record.clone()));
    assert!(staging.join(&record.asset).exists());
    assert!(kept.read_warning().is_some());
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

#[test]
fn a_launch_with_checks_off_leaves_an_update_a_helper_has_claimed() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let version = next_version();
    let record = stage_fake(&staging, &version);
    let (server, asked) = release_server(version.to_string(), 404);
    let claim = Staging::new(staging.clone()).begin_apply().unwrap();
    let mut app = launch_with_checks_off(dir.path(), &staging, &server, false);
    assert!(run_frames(&mut app, App::launch_update_check_done));
    quiet_frames(&mut app, 10);
    assert!(staging.join(&record.asset).exists());
    assert!(staging.join("staged.json").exists());
    drop(claim);
    assert_eq!(Staging::new(staging).read_record(), Some(record));
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

#[test]
fn an_install_for_every_user_offers_restart_to_update_and_applies_at_close() {
    for restart in [true, false] {
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
            Some(InstallMode::Installed { machine: true }),
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
            let output = probe.frame(Vec::new(), &mut run);
            let painted = ca_ui::testing::probe::painted_texts(&output).join(" ");
            assert!(!painted.contains("downloads page"), "{painted}");
            if restart {
                probe.click("Restart to update", &mut run).unwrap();
                probe.idle(&mut run);
            }
        }
        if !restart {
            app.run(Command::Exit, &context());
        }
        if app.asks_before_closing() {
            app.answer_exit(true);
        }
        assert!(run_frames(&mut app, App::is_closing));
        let started = Mutex::new(Vec::new());
        let start = |_: &Staging, relaunch: bool| {
            started.lock().unwrap().push(relaunch);
            Ok(())
        };
        assert!(finish_at_exit(&hook, &Staging::new(staging), &start));
        assert_eq!(*started.lock().unwrap(), vec![restart]);
    }
}

#[test]
fn a_check_that_is_not_due_schedules_the_next_one_at_the_due_time() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    let day = 24 * 60 * 60;
    let last = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 29 * day;
    std::fs::write(settings.join("update-check"), format!("{last}\n")).unwrap();
    let (server, asked) = release_server(next_version().to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(&server, &dir.path().join("stage"), None));
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    let mut options = app.options().options_or_default();
    options.tweaks.check_for_updates_days = 30;
    app.apply_options(options);
    assert!(run_frames(&mut app, |app| app
        .next_update_check_in()
        .is_some()));
    let wait = app.next_update_check_in().unwrap();
    assert!(wait <= Duration::from_secs(day + 60), "{wait:?}");
    assert!(wait >= Duration::from_secs(day - 60), "{wait:?}");
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

#[test]
fn a_manual_check_with_the_option_off_reports_and_downloads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let version = next_version();
    let (server, asked) = release_server(version.to_string(), 404);
    let mut app = app_in(dir.path());
    app.update_from(source(
        &server,
        &staging,
        Some(InstallMode::Installed { machine: false }),
    ));
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    let mut options = app.options().options_or_default();
    options.tweaks.check_for_updates = false;
    app.apply_options(options);
    assert!(run_frames(&mut app, App::launch_update_check_done));
    quiet_frames(&mut app, 5);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(app.accepts(Command::CheckForUpdates));
    app.run(Command::CheckForUpdates, &context());
    assert!(run_frames(&mut app, |app| matches!(
        app.update_notice(),
        Some(UpdateNotice::Newer(_))
    )));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(Staging::new(staging).read_record().is_none());
}

#[test]
fn a_failed_apply_is_reported_at_the_next_launch_with_the_downloads_page() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let version = next_version();
    let (server, asked) = release_server(version.to_string(), 404);
    write_warning(&staging, &server, &version);
    Staging::new(staging.clone()).write_apply_failure(
        "cannot place ca.exe: access denied; the earlier files are kept in C:\\p\\.update-backup-7",
    );
    check_just_done(dir.path());
    let mut app = app_in(dir.path());
    app.update_from(source(
        &server,
        &staging,
        Some(InstallMode::Installed { machine: false }),
    ));
    assert!(run_frames(&mut app, |app| app.update_notice().is_some()));
    let opened = Arc::new(Mutex::new(Vec::new()));
    app.set_spawner(Arc::new(Recorder(Arc::clone(&opened))));
    let mut probe = Probe::new(1280.0, 800.0);
    {
        let mut run = |ctx: &egui::Context| app.frame(ctx);
        probe.idle(&mut run);
        probe.idle(&mut run);
        let output = probe.frame(Vec::new(), &mut run);
        let painted = ca_ui::testing::probe::painted_texts(&output).join(" ");
        assert!(
            painted.contains(&format!("Version {version} could not be installed")),
            "{painted}"
        );
        assert!(painted.contains(".update-backup-7"), "{painted}");
        probe.click("OK", &mut run).unwrap();
        probe.idle(&mut run);
    }
    assert!(run_frames(&mut app, |_| !opened.lock().unwrap().is_empty()));
    assert!(opened.lock().unwrap()[0]
        .arguments
        .last()
        .unwrap()
        .ends_with(&format!("/tag/v{version}")));
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

#[test]
fn a_manual_check_while_another_copy_holds_the_staging_folder_does_not_say_current() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("stage");
    let (server, _) = release_server(next_version().to_string(), 404);
    let claim = Staging::new(staging.clone()).begin_apply().unwrap();
    let mut app = app_in(dir.path());
    app.update_from(source(
        &server,
        &staging,
        Some(InstallMode::Installed { machine: false }),
    ));
    check_just_done(dir.path());
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    assert!(run_frames(&mut app, App::launch_update_check_done));
    quiet_frames(&mut app, 5);
    assert!(app.accepts(Command::CheckForUpdates));
    app.run(Command::CheckForUpdates, &context());
    assert!(run_frames(&mut app, |app| matches!(
        app.update_notice(),
        Some(UpdateNotice::Message(_))
    )));
    let Some(UpdateNotice::Message(text)) = app.update_notice() else {
        panic!("{:?}", app.update_notice());
    };
    assert!(text.contains("Check again later"), "{text}");
    drop(claim);
}

#[cfg(windows)]
#[test]
fn the_program_file_carries_its_version_for_the_updater() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_compare-all")).unwrap();
    assert_eq!(
        ca_update::verify::embedded_version(&bytes).unwrap(),
        Version::parse(VERSION).unwrap()
    );
}

/// The version information as Windows reads it from the file. The elevation
/// prompt shows the description as the program name.
#[cfg(all(windows, target_env = "msvc"))]
#[test]
fn the_program_file_names_the_product_and_its_version() {
    let root = ca_update::system::system_root().unwrap();
    let output = ca_io::host_command::host_command(
        root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    )
    .args(["-NoProfile", "-NonInteractive", "-Command"])
    .arg(
        "$v=(Get-Item -LiteralPath $env:CA_TARGET).VersionInfo;\
         $v.FileDescription;$v.ProductName;$v.FileVersion;$v.ProductVersion;\
         $v.OriginalFilename",
    )
    .env("CA_TARGET", env!("CARGO_BIN_EXE_compare-all"))
    .output()
    .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        [
            "Compare All",
            "Compare All",
            VERSION,
            VERSION,
            "compare-all.exe"
        ]
    );
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
    assert!(run_frames(&mut app, App::launch_update_check_done));
    quiet_frames(&mut app, 30);
    assert!(app.update_notice().is_none());
    assert!(Staging::new(staging_path).read_warning().is_none());
}

/// The work area of a 1920 by 1040 monitor whose taskbar takes the bottom 40.
#[allow(clippy::unnecessary_wraps)]
fn work_area(_outer: egui::Rect, _pixels_per_point: f32) -> Option<egui::Rect> {
    Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(1920.0, 1040.0),
    ))
}

#[derive(Clone, Copy, Debug)]
enum Shown {
    Newer,
    Ready,
    NotDownloaded,
    NotInstalled,
}

impl Shown {
    const ALL: [Self; 4] = [
        Self::Newer,
        Self::Ready,
        Self::NotDownloaded,
        Self::NotInstalled,
    ];

    /// The button labels the notice carries, and the one that is pressed.
    fn buttons(self) -> (&'static [&'static str], &'static str) {
        match self {
            Self::Newer => (&["Open Release Page"], "Open Release Page"),
            Self::Ready => (&["Restart to update"], "Restart to update"),
            Self::NotDownloaded | Self::NotInstalled => (&["OK", "Cancel"], "OK"),
        }
    }
}

/// An app that shows the update notice of `shown`, the server behind it, and
/// the commands its Open With spawner recorded.
fn app_showing(dir: &Path, shown: Shown) -> (App, HttpTestServer, Arc<Mutex<Vec<LaunchCommand>>>) {
    let staging = dir.join("stage");
    let version = next_version();
    let mode = Some(InstallMode::Installed { machine: false });
    let status = if matches!(shown, Shown::NotDownloaded) {
        403
    } else {
        404
    };
    let (server, _) = release_server(version.to_string(), status);
    match shown {
        Shown::Ready => {
            stage_fake(&staging, &version);
            check_just_done(dir);
        }
        Shown::NotInstalled => {
            write_warning(&staging, &server, &version);
            Staging::new(staging.clone()).write_apply_failure("cannot place ca.exe: access denied");
            check_just_done(dir);
        }
        Shown::Newer | Shown::NotDownloaded => {}
    }
    let mut app = app_in(dir);
    app.update_from(source(&server, &staging, mode));
    let opened = Arc::new(Mutex::new(Vec::new()));
    app.set_spawner(Arc::new(Recorder(Arc::clone(&opened))));
    app.use_work_area_source(work_area);
    if matches!(shown, Shown::Newer) {
        assert!(run_frames(&mut app, |app| app.options().is_ready()));
        let mut options = app.options().options_or_default();
        options.tweaks.check_for_updates = false;
        app.apply_options(options);
        assert!(run_frames(&mut app, App::launch_update_check_done));
        app.run(Command::CheckForUpdates, &context());
    }
    assert!(run_frames(&mut app, |app| app.update_notice().is_some()));
    (app, server, opened)
}

/// The state of a native window whose client area starts at `corner` and has
/// the size of the egui screen.
fn window_at(corner: egui::Pos2, size: egui::Vec2) -> egui::ViewportInfo {
    let inner = egui::Rect::from_min_size(corner, size);
    let outer = egui::Rect::from_min_max(
        inner.min - egui::vec2(8.0, 31.0),
        inner.max + egui::vec2(8.0, 8.0),
    );
    egui::ViewportInfo {
        inner_rect: Some(inner),
        outer_rect: Some(outer),
        ..egui::ViewportInfo::default()
    }
}

#[test]
fn every_update_button_stays_inside_the_part_of_the_window_that_shows() {
    let size = egui::vec2(800.0, 600.0);
    // Client area corners that leave the bottom, the right edge or both
    // partly below or beyond the 1920 by 1040 work area.
    let corners = [
        egui::pos2(100.0, 700.0),
        egui::pos2(1320.0, 100.0),
        egui::pos2(1320.0, 700.0),
    ];
    for shown in Shown::ALL {
        for corner in corners {
            let dir = tempfile::tempdir().unwrap();
            let (mut app, _server, _opened) = app_showing(dir.path(), shown);
            let visible = egui::Rect::from_min_max(
                egui::Pos2::ZERO,
                egui::pos2(
                    (1920.0 - corner.x).min(size.x),
                    (1040.0 - corner.y).min(size.y),
                ),
            );
            let mut probe = Probe::new(size.x, size.y);
            probe.set_viewport(window_at(corner, size));
            let mut run = |ctx: &egui::Context| app.frame(ctx);
            probe.idle(&mut run);
            probe.idle(&mut run);
            probe.idle(&mut run);
            for label in shown.buttons().0 {
                let control = probe.find(label).unwrap();
                assert!(
                    visible.contains_rect(control.rect),
                    "{shown:?} at {corner:?}: {label:?} is at {:?}, visible area is {visible:?}",
                    control.rect
                );
            }
        }
    }
}

#[test]
fn every_update_button_answers_a_click_in_a_small_window_that_hangs_off_screen() {
    let size = egui::vec2(640.0, 400.0);
    let corner = egui::pos2(1400.0, 800.0);
    for shown in Shown::ALL {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, _server, opened) = app_showing(dir.path(), shown);
        let mut probe = Probe::new(size.x, size.y);
        probe.set_viewport(window_at(corner, size));
        {
            let mut run = |ctx: &egui::Context| app.frame(ctx);
            probe.idle(&mut run);
            probe.idle(&mut run);
            probe.idle(&mut run);
            for label in shown.buttons().0 {
                assert!(probe.find(label).is_ok(), "{shown:?}: no {label:?}");
            }
            probe.click(shown.buttons().1, &mut run).unwrap();
            probe.idle(&mut run);
        }
        match shown {
            Shown::Ready => assert!(run_frames(&mut app, App::is_closing)),
            Shown::Newer | Shown::NotDownloaded | Shown::NotInstalled => {
                assert!(
                    run_frames(&mut app, |_| !opened.lock().unwrap().is_empty()),
                    "{shown:?}: the release page did not open"
                );
            }
        }
    }
}

#[test]
fn the_cancel_button_closes_the_notice_in_a_window_that_hangs_off_screen() {
    let size = egui::vec2(640.0, 400.0);
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _server, opened) = app_showing(dir.path(), Shown::NotDownloaded);
    let mut probe = Probe::new(size.x, size.y);
    probe.set_viewport(window_at(egui::pos2(1400.0, 800.0), size));
    {
        let mut run = |ctx: &egui::Context| app.frame(ctx);
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe.click("Cancel", &mut run).unwrap();
        probe.idle(&mut run);
    }
    assert!(app.update_notice().is_none());
    assert!(opened.lock().unwrap().is_empty());
}

#[test]
fn a_window_that_opens_below_the_work_area_is_asked_to_move_up_once() {
    let size = egui::vec2(1280.0, 800.0);
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    app.use_work_area_source(work_area);
    let mut probe = Probe::new(size.x, size.y);
    probe.set_viewport(window_at(egui::pos2(242.0, 265.0), size));
    let mut run = |ctx: &egui::Context| app.frame(ctx);
    let output = probe.frame(Vec::new(), &mut run);
    let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
    // The outer rectangle is 839 tall and starts at 234; it must end at 1040.
    assert!(
        commands.contains(&egui::ViewportCommand::OuterPosition(egui::pos2(
            234.0,
            1040.0 - 839.0
        ))),
        "{commands:?}"
    );
    assert!(!commands
        .iter()
        .any(|command| matches!(command, egui::ViewportCommand::InnerSize(_))));
    probe.set_viewport(window_at(egui::pos2(242.0, 232.0), size));
    let output = probe.frame(Vec::new(), &mut run);
    let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::OuterPosition(_))),
        "{commands:?}"
    );
}

#[test]
fn a_window_taller_than_the_work_area_is_asked_to_shrink() {
    let size = egui::vec2(1280.0, 800.0);
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    app.use_work_area_source(|_, _| {
        Some(egui::Rect::from_min_size(
            egui::pos2(1920.0, 0.0),
            egui::vec2(1366.0, 728.0),
        ))
    });
    let mut probe = Probe::new(size.x, size.y);
    probe.set_viewport(window_at(egui::pos2(2000.0, 100.0), size));
    let mut run = |ctx: &egui::Context| app.frame(ctx);
    let output = probe.frame(Vec::new(), &mut run);
    let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
    // Outer height 839 against 728 cuts 111; the client area keeps the rest.
    assert!(
        commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(
            1280.0,
            800.0 - 111.0
        ))),
        "{commands:?}"
    );
    assert!(
        commands.contains(&egui::ViewportCommand::OuterPosition(egui::pos2(
            1990.0, 0.0
        ))),
        "{commands:?}"
    );
}
