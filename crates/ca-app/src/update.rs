//! Update checks, downloads and the apply at exit.
//!
//! The check runs on a worker. It reads the newest release, and on Windows
//! downloads, verifies and stages the matching file without a prompt. A check
//! that fails is silent unless the user asked for it. A staged update is
//! applied by a helper after the program ends through a normal close; the
//! launch never applies and never waits on an update.

use ca_ui::worker::{Emitter, Job, Terminal};
use ca_update::flow::{Endpoints, Job as FlowJob};
use ca_update::mode::InstallMode;
use ca_update::net::{HttpTransport, ProxyConfig};
use ca_update::staging::Staging;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use ca_update::Version;

/// The address that names the newest release.
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/jasonulbright/compare-all/releases/latest";

/// How long one check may take before it is given up.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// True on platforms where the program downloads and applies updates.
pub const AUTOMATIC: bool = cfg!(windows);

/// File that holds the time of the last check, in seconds since 1970.
const STAMP_FILE: &str = "update-check";
/// File that holds the reason the last check failed.
const LOG_FILE: &str = "update.log";

/// Seconds in one day.
const DAY: u64 = 24 * 60 * 60;

/// A release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version.
    pub version: Version,
    /// The page that holds its notes and its files.
    pub page: String,
}

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A newer release exists that this build does not download.
    Newer(Release),
    /// A newer release is downloaded, verified and staged.
    Staged(Release),
    /// A verified update was staged by an earlier run and still waits.
    Waiting(Release),
    /// An earlier run could not download this release.
    EarlierFailure(Release),
    /// A newer release exists and the download failed.
    DownloadFailed(Release, String),
    /// The running build is the newest release or newer.
    Current,
    /// The interval since the last check has not passed.
    NotDue,
    /// The check failed; the reason is in the log file.
    Failed(String),
    /// The check was stopped.
    Cancelled,
}

impl Terminal for Outcome {
    fn is_terminal(&self) -> bool {
        !matches!(self, Self::Waiting(_) | Self::EarlierFailure(_))
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// Where a window gets releases from and what it does with them.
#[derive(Debug, Clone)]
pub struct Source {
    /// The release locations.
    pub endpoints: Endpoints,
    /// How the program was installed. None only reports a newer release.
    pub mode: Option<InstallMode>,
    /// The staging folder.
    pub staging: PathBuf,
    /// Accept plain HTTP addresses. For tests against a local server.
    pub allow_plain_http: bool,
}

impl Source {
    /// The public releases. Windows downloads and stages; other platforms
    /// report a newer release only.
    #[must_use]
    pub fn published(settings: &Path) -> Self {
        Self {
            endpoints: Endpoints::github(),
            mode: AUTOMATIC
                .then(|| std::env::current_exe().ok())
                .flatten()
                .map(|exe| ca_update::mode::detect(&exe)),
            staging: staging_directory(settings),
            allow_plain_http: false,
        }
    }

    /// A source that reads the release document at `url` and downloads nothing.
    #[must_use]
    pub fn notify_only(url: String, settings: &Path) -> Self {
        let mut endpoints = Endpoints::github();
        endpoints.latest = url;
        Self {
            endpoints,
            mode: None,
            staging: settings.join("updates"),
            allow_plain_http: true,
        }
    }
}

/// The folder staged updates live in. It sits in the per-user local data
/// folder, never beside the program, so a portable folder gets no extra files.
#[must_use]
pub fn staging_directory(settings: &Path) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || settings.join("updates"),
            |base| PathBuf::from(base).join("compare-all").join("updates"),
        )
}

/// One check to run.
#[derive(Debug, Clone)]
pub struct Check {
    /// Where releases come from.
    pub source: Source,
    /// The running build.
    pub current: Version,
    /// The value of the user agent header.
    pub user_agent: String,
    /// Folder that holds the time of the last check and the log file.
    pub state: PathBuf,
    /// Days between checks. Zero checks at every start.
    pub days: u32,
    /// Check even when the interval has not passed.
    pub forced: bool,
    /// How long a request may take.
    pub timeout: Duration,
    /// Report the earlier failure and the staged update before the check.
    pub report_pending: bool,
}

impl Check {
    /// A check of `source` against `current`.
    #[must_use]
    pub fn new(source: Source, current: Version, state: PathBuf, days: u32, forced: bool) -> Self {
        Self {
            source,
            current,
            user_agent: user_agent(&current.to_string()),
            state,
            days,
            forced,
            timeout: TIMEOUT,
            report_pending: false,
        }
    }
}

/// The user agent header: the application and its version, nothing else.
#[must_use]
pub fn user_agent(version: &str) -> String {
    format!("compare-all/{version}")
}

/// True when a check is due `days` after the one at `last`.
///
/// A last check dated after `now` means the clock moved back, so a check is
/// due rather than delayed until the clock catches up.
#[must_use]
pub fn is_due(last: Option<SystemTime>, now: SystemTime, days: u32) -> bool {
    let Some(last) = last else {
        return true;
    };
    match now.duration_since(last) {
        Ok(elapsed) => elapsed.as_secs() >= u64::from(days) * DAY,
        Err(_) => true,
    }
}

/// The release a GitHub release document names.
///
/// # Errors
///
/// Returns why the document is not a release of this application.
pub fn parse_release(body: &[u8]) -> Result<Version, String> {
    ca_update::flow::parse_release(body)
}

/// Run `check` now: read the last check time, ask the server, download when
/// the source allows it, and record the attempt. Performs network and disk
/// I/O; call it on a worker.
#[must_use]
pub fn run(check: &Check, now: SystemTime, cancel: &Arc<AtomicBool>) -> Outcome {
    if !check.forced && !is_due(read_stamp(&check.state), now, check.days) {
        return Outcome::NotDue;
    }
    // The attempt is recorded before the request, so a check that fails or
    // hangs still waits one interval before the next one.
    write_stamp(&check.state, now);
    let outcome = match fetch(check, cancel) {
        Ok(outcome) => outcome,
        Err(_) if cancel.load(Ordering::SeqCst) => Outcome::Cancelled,
        Err(reason) => Outcome::Failed(reason),
    };
    if let Outcome::Failed(reason) = &outcome {
        write_log(&check.state, now, reason);
    }
    outcome
}

/// Start `check` on a worker.
pub fn spawn(check: Check, notify: Arc<dyn Fn() + Send + Sync>) -> Job<Outcome> {
    Job::spawn_notifying(
        move |emitter: &Emitter<Outcome>, cancel| {
            let flag = cancel.as_fs().as_flag();
            if check.report_pending {
                Staging::new(check.source.staging.clone()).sweep_helpers();
                for pending in pending(&check) {
                    emitter.send(pending);
                }
            }
            emitter.send(run(&check, SystemTime::now(), &flag));
        },
        notify,
    )
}

/// What an earlier run left: a staged update that waits, or a release that
/// could not be downloaded. Reads local files only.
fn pending(check: &Check) -> Vec<Outcome> {
    let staging = Staging::new(check.source.staging.clone());
    let mut out = Vec::new();
    let release = |version: Version| Release {
        page: check.source.endpoints.page(&version),
        version,
    };
    if let Some(version) = staging
        .read_record()
        .and_then(|record| record.parsed_version())
        .filter(|version| *version > check.current)
    {
        out.push(Outcome::Waiting(release(version)));
    }
    if let Some(warning) = staging.read_warning() {
        if let Some(version) = warning.parsed_version().filter(|v| *v > check.current) {
            out.push(Outcome::EarlierFailure(Release {
                version,
                page: warning.page,
            }));
        }
    }
    out
}

fn fetch(check: &Check, cancel: &Arc<AtomicBool>) -> Result<Outcome, String> {
    let staging = Staging::new(check.source.staging.clone());
    let mut transport = HttpTransport::new(
        check.user_agent.clone(),
        ProxyConfig::system(),
        check.timeout,
    );
    if check.source.allow_plain_http {
        transport = transport.allowing_plain_http();
    }
    let signer = ca_update::signature::system_verifier();
    let job = FlowJob {
        transport: &transport,
        signer: signer.as_ref(),
        staging: &staging,
        mode: check.source.mode,
        current: check.current,
        endpoints: &check.source.endpoints,
        signer_name: ca_update::signature::RELEASE_SIGNER,
        cancel,
    };
    Ok(match ca_update::flow::run(&job)? {
        ca_update::flow::Outcome::Current => Outcome::Current,
        ca_update::flow::Outcome::Newer { version, page } => {
            Outcome::Newer(Release { version, page })
        }
        ca_update::flow::Outcome::Staged { version } => Outcome::Staged(Release {
            page: check.source.endpoints.page(&version),
            version,
        }),
        ca_update::flow::Outcome::DownloadFailed {
            version,
            page,
            reason,
        } => Outcome::DownloadFailed(Release { version, page }, reason),
    })
}

fn read_stamp(state: &Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(state.join(STAMP_FILE)).ok()?;
    let seconds: u64 = text.trim().parse().ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn write_stamp(state: &Path, now: SystemTime) {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let _ = std::fs::create_dir_all(state);
    let _ = ca_io::write_atomic(&state.join(STAMP_FILE), format!("{seconds}\n").as_bytes());
}

/// Keep the reason of the last failure. The file holds one line, so it never
/// grows.
fn write_log(state: &Path, now: SystemTime, reason: &str) {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let _ = std::fs::create_dir_all(state);
    let line = reason.replace(['\r', '\n'], " ");
    let _ = ca_io::write_atomic(
        &state.join(LOG_FILE),
        format!("{seconds} update check failed: {line}\n").as_bytes(),
    );
}

/// What the window tells the process about how it ended.
#[derive(Debug, Default)]
pub struct ExitHook {
    /// The window ended through its normal close path.
    pub normal_close: AtomicBool,
    /// The close was asked for by "Restart to update".
    pub relaunch: AtomicBool,
}

/// After the window ended: when it ended through a normal close and a staged
/// update waits, start the helper that applies it. A crash or a forced end
/// leaves `normal_close` unset, so the update stays staged for the next normal
/// close. Returns true when a helper was started.
pub fn finish_at_exit(
    hook: &ExitHook,
    staging: &Staging,
    start: &dyn Fn(&Staging, bool) -> Result<(), String>,
) -> bool {
    if !hook.normal_close.load(Ordering::SeqCst) {
        return false;
    }
    let current = Version::parse(crate::VERSION);
    let newer = staging
        .read_record()
        .and_then(|record| record.parsed_version())
        .is_some_and(|staged| current.is_none_or(|current| staged > current));
    if !newer {
        return false;
    }
    start(staging, hook.relaunch.load(Ordering::SeqCst)).is_ok()
}

/// [`finish_at_exit`] with the real helper, for the staging folder of
/// `settings`. Does nothing off Windows.
pub fn apply_at_exit(hook: &ExitHook, settings: &Path) -> bool {
    if !AUTOMATIC {
        return false;
    }
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let staging = Staging::new(staging_directory(settings));
    finish_at_exit(hook, &staging, &|staging, relaunch| {
        ca_update::apply::spawn_helper(staging, &executable, relaunch, &[])
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{is_due, parse_release, run, Check, Outcome, Source, Version, DAY};
    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    use ca_vfs::testing::http_server::{HttpTestServer, Reply, Request};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    const CURRENT: &str = "2026.09.25.0002";

    fn release(tag: &str) -> Vec<u8> {
        format!(r#"{{"tag_name":"{tag}","html_url":"https://example.invalid/","body":"notes"}}"#)
            .into_bytes()
    }

    fn check(server: &HttpTestServer, state: &std::path::Path) -> Check {
        Check {
            source: Source::notify_only(
                format!(
                    "{}/repos/jasonulbright/compare-all/releases/latest",
                    server.url()
                ),
                state,
            ),
            current: Version::parse(CURRENT).unwrap(),
            user_agent: super::user_agent(CURRENT),
            state: state.to_path_buf(),
            days: 7,
            forced: false,
            timeout: Duration::from_secs(10),
            report_pending: false,
        }
    }

    fn serving(body: Vec<u8>) -> (HttpTestServer, Arc<Mutex<Vec<Request>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let server = HttpTestServer::start(
            move |request| {
                log.lock().unwrap().push(request.clone());
                Reply::body(200, "application/json", body.clone())
            },
            None,
        );
        (server, seen)
    }

    #[test]
    fn versions_compare_by_their_four_numbers() {
        let parse = |text| Version::parse(text).unwrap();
        assert!(parse("v2026.09.25.0003") > parse("2026.09.25.0002"));
        assert!(parse("v2026.10.1.1") > parse("v2026.9.30.9"));
        assert!(parse("v2027.01.01.0001") > parse("v2026.12.31.9999"));
        assert_eq!(parse("v2026.09.25.0002"), parse("2026.9.25.2"));
        assert_eq!(parse("v2026.9.25.2").to_string(), "2026.09.25.0002");
        for bad in [
            "",
            "v",
            "2026.09.25",
            "2026.09.25.1.2",
            "2026.09.x.1",
            "2026..25.1",
            "v-1.1.1.1",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version::parse(crate::VERSION).is_some());
    }

    #[test]
    fn a_check_is_due_once_the_interval_has_passed() {
        let now = SystemTime::now();
        let ago = |seconds| now - Duration::from_secs(seconds);
        assert!(is_due(None, now, 7));
        assert!(!is_due(Some(ago(DAY)), now, 7));
        assert!(!is_due(Some(ago(7 * DAY - 1)), now, 7));
        assert!(is_due(Some(ago(7 * DAY)), now, 7));
        assert!(is_due(Some(now + Duration::from_secs(DAY)), now, 7));
        assert!(is_due(Some(now), now, 0));
    }

    #[test]
    fn a_newer_tag_is_reported_with_its_page_and_the_request_carries_only_the_user_agent() {
        let state = tempfile::tempdir().unwrap();
        let (server, seen) = serving(release("v2026.09.26.0001"));
        let outcome = run(
            &check(&server, state.path()),
            SystemTime::now(),
            &no_cancel(),
        );
        let Outcome::Newer(found) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(found.version.to_string(), "2026.09.26.0001");
        assert_eq!(
            found.page,
            "https://github.com/jasonulbright/compare-all/releases/tag/v2026.09.26.0001"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert_eq!(request.method, "GET");
        assert!(request.body.is_empty());
        let agents: Vec<&str> = request
            .headers
            .iter()
            .filter(|(name, _)| name == "user-agent")
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(agents, vec!["compare-all/2026.09.25.0002"]);
        for (name, _) in &request.headers {
            assert!(
                [
                    "host",
                    "connection",
                    "accept",
                    "accept-encoding",
                    "user-agent"
                ]
                .contains(&name.as_str()),
                "unexpected header {name}"
            );
        }
    }

    #[test]
    fn the_same_or_an_older_tag_is_not_newer() {
        for tag in [CURRENT, "v2026.09.25.0001", "v2025.12.31.9999"] {
            let state = tempfile::tempdir().unwrap();
            let (server, _) = serving(release(tag));
            let outcome = run(
                &check(&server, state.path()),
                SystemTime::now(),
                &no_cancel(),
            );
            assert_eq!(outcome, Outcome::Current, "{tag}");
        }
    }

    #[test]
    fn a_check_inside_the_interval_asks_nothing_unless_forced() {
        let state = tempfile::tempdir().unwrap();
        let (server, seen) = serving(release("v2026.09.26.0001"));
        let mut check = check(&server, state.path());
        let now = SystemTime::now();
        assert!(matches!(run(&check, now, &no_cancel()), Outcome::Newer(_)));
        let later = now + Duration::from_secs(DAY);
        assert_eq!(run(&check, later, &no_cancel()), Outcome::NotDue);
        assert_eq!(seen.lock().unwrap().len(), 1);
        check.forced = true;
        assert!(matches!(
            run(&check, later, &no_cancel()),
            Outcome::Newer(_)
        ));
        check.forced = false;
        let past = now + Duration::from_secs(8 * DAY);
        assert!(matches!(run(&check, past, &no_cancel()), Outcome::Newer(_)));
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn malformed_json_fails_into_the_log_only() {
        for body in [
            b"not json".to_vec(),
            br#"{"name":"no tag"}"#.to_vec(),
            release("latest"),
        ] {
            let state = tempfile::tempdir().unwrap();
            let (server, _) = serving(body);
            let outcome = run(
                &check(&server, state.path()),
                SystemTime::now(),
                &no_cancel(),
            );
            assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
            let log = std::fs::read_to_string(state.path().join(super::LOG_FILE)).unwrap();
            assert!(log.contains("update check failed"));
            assert_eq!(log.lines().count(), 1);
        }
        assert!(parse_release(br#"{"tag_name":"v2026.09.25.0002"}"#).is_ok());
    }

    #[test]
    fn a_refused_connection_fails_quietly() {
        let state = tempfile::tempdir().unwrap();
        let (server, _) = serving(Vec::new());
        let mut check = check(&server, state.path());
        drop(server);
        check.source.endpoints.latest = "http://127.0.0.1:9/releases/latest".to_owned();
        assert!(matches!(
            run(&check, SystemTime::now(), &no_cancel()),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn a_slow_server_is_left_when_the_check_is_cancelled() {
        let state = tempfile::tempdir().unwrap();
        let answered = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&answered);
        let server = HttpTestServer::start(
            move |_| {
                std::thread::sleep(Duration::from_secs(8));
                count.fetch_add(1, Ordering::SeqCst);
                Reply::body(200, "application/json", release("v2026.09.26.0001"))
            },
            None,
        );
        let mut check = check(&server, state.path());
        check.timeout = Duration::from_secs(30);
        let cancel = no_cancel();
        let stopper = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            stopper.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let outcome = run(&check, SystemTime::now(), &cancel);
        assert_eq!(outcome, Outcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(6));
        assert_eq!(answered.load(Ordering::SeqCst), 0);
        assert!(!state.path().join(super::LOG_FILE).exists());
    }
}
