//! Asks GitHub Releases whether a newer build exists.
//!
//! The check runs on a worker, sends one GET request whose only caller header
//! is the user agent, and never downloads or installs anything. A
//! failure is written to a log file in the settings directory and is otherwise
//! silent.

use ca_ui::worker::{Emitter, Job, Terminal};
use ca_vfs::cancel::Cancel;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The address that names the newest published release.
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/jasonulbright/compare-all/releases/latest";

/// The page of one release, before its tag.
const RELEASE_PAGE: &str = "https://github.com/jasonulbright/compare-all/releases/tag/";

/// How long one check may take before it is given up.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Most bytes a release document may hold. The document is small; a larger
/// reply is not one.
const MAX_BODY: u64 = 1 << 20;

/// File that holds the time of the last check, in seconds since 1970.
const STAMP_FILE: &str = "update-check";
/// File that holds the reason the last check failed.
const LOG_FILE: &str = "update.log";

/// Seconds in one day.
const DAY: u64 = 24 * 60 * 60;

/// A build version: year, month, day and build number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u32; 4]);

impl Version {
    /// Parse `YYYY.MM.DD.NNNN`, with or without a leading `v`.
    ///
    /// The four parts compare as numbers, so `2026.10.1.1` is newer than
    /// `2026.9.30.9`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let mut parts = [0u32; 4];
        let mut pieces = text.split('.');
        for part in &mut parts {
            let piece = pieces.next()?;
            if piece.is_empty() || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *part = piece.parse().ok()?;
        }
        if pieces.next().is_some() {
            return None;
        }
        Some(Self(parts))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [year, month, day, build] = self.0;
        write!(f, "{year:04}.{month:02}.{day:02}.{build:04}")
    }
}

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
    /// A newer release exists.
    Newer(Release),
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
        true
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// One check to run.
#[derive(Debug, Clone)]
pub struct Check {
    /// The address of the release document.
    pub url: String,
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
    /// How long the request may take.
    pub timeout: Duration,
}

impl Check {
    /// A check of the published releases against `current`.
    #[must_use]
    pub fn published(current: Version, state: PathBuf, days: u32, forced: bool) -> Self {
        Self {
            url: RELEASES_URL.to_owned(),
            current,
            user_agent: user_agent(&current.to_string()),
            state,
            days,
            forced,
            timeout: TIMEOUT,
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
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| format!("the reply is not JSON: {error}"))?;
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "the reply names no tag".to_owned())?;
    Version::parse(tag).ok_or_else(|| format!("the tag {tag:?} is not a version"))
}

/// Run `check` now: read the last check time, ask the server, and record the
/// attempt. Performs network and disk I/O; call it on a worker.
#[must_use]
pub fn run(check: &Check, now: SystemTime, cancel: &Cancel) -> Outcome {
    if !check.forced && !is_due(read_stamp(&check.state), now, check.days) {
        return Outcome::NotDue;
    }
    // The attempt is recorded before the request, so a check that fails or
    // hangs still waits one interval before the next one.
    write_stamp(&check.state, now);
    let outcome = match fetch(check, cancel) {
        Ok(latest) if latest > check.current => Outcome::Newer(Release {
            version: latest,
            page: format!("{RELEASE_PAGE}v{latest}"),
        }),
        Ok(_) => Outcome::Current,
        Err(_) if cancel.is_cancelled() => Outcome::Cancelled,
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
            let cancel = Cancel::from_flag(cancel.as_fs().as_flag());
            emitter.send(run(&check, SystemTime::now(), &cancel));
        },
        notify,
    )
}

/// Ask the server for the newest release.
fn fetch(check: &Check, cancel: &Cancel) -> Result<Version, String> {
    use ca_vfs::remote::http::{HttpClient, RequestBody, Url};
    let url = Url::parse(&check.url).map_err(|error| error.to_string())?;
    let client = HttpClient::new(
        &ca_vfs::remote::tls::TlsOptions::default(),
        None,
        check.timeout,
    )
    .map_err(|error| error.to_string())?;
    let headers = [("User-Agent".to_owned(), check.user_agent.clone())];
    let mut response = client
        .send("GET", &url, &headers, RequestBody::Empty, cancel)
        .map_err(|error| error.to_string())?;
    if !response.is_success() {
        return Err(format!("the server answered {}", response.status));
    }
    let limits = ca_vfs::limits::Limits {
        max_entry_bytes: MAX_BODY,
        max_archive_bytes: MAX_BODY,
        ..ca_vfs::limits::Limits::default()
    };
    let body = response
        .read_body(&limits, cancel)
        .map_err(|error| error.to_string())?;
    parse_release(&body)
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{is_due, parse_release, run, Check, Outcome, Version, DAY};
    use ca_vfs::cancel::Cancel;
    use ca_vfs::testing::http_server::{HttpTestServer, Reply, Request};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    const CURRENT: &str = "2026.09.25.0002";

    fn release(tag: &str) -> Vec<u8> {
        format!(r#"{{"tag_name":"{tag}","html_url":"https://example.invalid/","body":"notes"}}"#)
            .into_bytes()
    }

    fn check(server: &HttpTestServer, state: &std::path::Path) -> Check {
        Check {
            url: format!(
                "{}/repos/jasonulbright/compare-all/releases/latest",
                server.url()
            ),
            current: Version::parse(CURRENT).unwrap(),
            user_agent: super::user_agent(CURRENT),
            state: state.to_path_buf(),
            days: 7,
            forced: false,
            timeout: Duration::from_secs(10),
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
            &Cancel::new(),
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
                ["host", "connection", "accept-encoding", "user-agent"].contains(&name.as_str()),
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
                &Cancel::new(),
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
        assert!(matches!(
            run(&check, now, &Cancel::new()),
            Outcome::Newer(_)
        ));
        let later = now + Duration::from_secs(DAY);
        assert_eq!(run(&check, later, &Cancel::new()), Outcome::NotDue);
        assert_eq!(seen.lock().unwrap().len(), 1);
        check.forced = true;
        assert!(matches!(
            run(&check, later, &Cancel::new()),
            Outcome::Newer(_)
        ));
        check.forced = false;
        let past = now + Duration::from_secs(8 * DAY);
        assert!(matches!(
            run(&check, past, &Cancel::new()),
            Outcome::Newer(_)
        ));
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
                &Cancel::new(),
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
        check.url = "http://127.0.0.1:9/releases/latest".to_owned();
        assert!(matches!(
            run(&check, SystemTime::now(), &Cancel::new()),
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
        let cancel = Cancel::new();
        let stopper = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            stopper.cancel();
        });
        let started = Instant::now();
        let outcome = run(&check, SystemTime::now(), &cancel);
        assert_eq!(outcome, Outcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(6));
        assert_eq!(answered.load(Ordering::SeqCst), 0);
        assert!(!state.path().join(super::LOG_FILE).exists());
    }
}
