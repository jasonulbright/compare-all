//! Read-only Subversion repositories reached through the `svn` client.
//!
//! The client reads its normal user configuration and credential cache. This
//! adapter never passes a password on the process command line and always
//! disables interactive prompts and new credential caching.

use command_group::{CommandGroup, GroupChild};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use quick_xml::events::Event;

use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsEntry};
use crate::error::{VfsError, VfsResult};
use crate::fs::{Capabilities, FileSystem, OpenFile};
use crate::limits::{materialize, Budget, Limits};
use crate::path::VfsPath;
use crate::remote::profile::SubversionProfile;
use crate::remote::RemoteContext;

/// Most XML one immediate-folder listing can produce.
const MAX_LISTING_BYTES: u64 = 64 * 1024 * 1024;
/// Most rows one Subversion folder can name.
const MAX_LISTED: usize = 1_000_000;
/// Most diagnostic text kept from the external client.
const MAX_DIAGNOSTIC_BYTES: usize = 32 * 1024;
/// Polling interval used while an external client runs.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A read-only view of one Subversion URL.
pub struct SubversionFs {
    base_url: String,
    username: String,
    revision: Option<u64>,
    timeout: Duration,
    limits: Limits,
    runner: Arc<dyn CommandRunner>,
    listings: Mutex<BTreeMap<VfsPath, Vec<VfsEntry>>>,
    raw_paths: Mutex<BTreeMap<VfsPath, Vec<String>>>,
}

impl std::fmt::Debug for SubversionFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubversionFs")
            .field("base_url", &self.base_url)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl SubversionFs {
    /// Open the read-only repository described by `profile`.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] when `svn` is unavailable or a
    /// password reference cannot be used safely, and the client's error when
    /// it cannot list the repository root.
    pub fn connect(
        profile: &SubversionProfile,
        context: &RemoteContext,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        let fs = Self::with_runner(profile, context, Arc::new(ProcessRunner::default()))?;
        fs.list(&VfsPath::root(), cancel)?;
        Ok(fs)
    }

    fn with_runner(
        profile: &SubversionProfile,
        context: &RemoteContext,
        runner: Arc<dyn CommandRunner>,
    ) -> VfsResult<Self> {
        validate_url(&profile.url)?;
        if !profile.password.is_empty() {
            return Err(VfsError::unsupported(
                "Subversion passwords are not passed to the svn process because command-line arguments can expose them; configure the svn client's credential provider instead",
            ));
        }
        if profile.username.chars().any(char::is_control) {
            return Err(VfsError::protocol(
                "the Subversion username contains a control character",
            ));
        }
        let timeout = context
            .call_timeout
            .min(Duration::from_secs(context.limits.max_helper_seconds));
        Ok(Self {
            base_url: trim_trailing_slashes(&profile.url),
            username: profile.username.clone(),
            revision: profile.revision,
            timeout,
            limits: context.limits,
            runner,
            listings: Mutex::new(BTreeMap::new()),
            raw_paths: Mutex::new(BTreeMap::new()),
        })
    }

    fn url(&self, path: &VfsPath) -> String {
        self.url_components(path.components())
    }

    fn url_for(&self, path: &VfsPath) -> String {
        let raw_components = self
            .raw_paths
            .lock()
            .ok()
            .and_then(|paths| paths.get(path).cloned());
        match raw_components {
            Some(components) => self.url_components(components.iter().map(String::as_str)),
            None => self.url(path),
        }
    }

    fn url_components<'a>(&self, components: impl Iterator<Item = &'a str>) -> String {
        let mut target = self.base_url.clone();
        for component in components {
            if !target.ends_with('/') {
                target.push('/');
            }
            target.push_str(&encode_component(component));
        }
        target
    }

    fn list_args(&self, path: &VfsPath) -> Vec<OsString> {
        let mut args = self.common_args();
        args.extend([
            OsString::from("list"),
            OsString::from("--xml"),
            OsString::from("--depth"),
            OsString::from("immediates"),
        ]);
        self.add_revision(&mut args);
        args.push(OsString::from(self.url_for(path)));
        args
    }

    fn cat_args(&self, path: &VfsPath) -> Vec<OsString> {
        let mut args = self.common_args();
        args.push(OsString::from("cat"));
        self.add_revision(&mut args);
        args.push(OsString::from(self.url_for(path)));
        args
    }

    fn common_args(&self) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("--non-interactive"),
            OsString::from("--no-auth-cache"),
        ];
        if !self.username.is_empty() {
            args.push(OsString::from(format!("--username={}", self.username)));
        }
        args
    }

    fn add_revision(&self, args: &mut Vec<OsString>) {
        if let Some(revision) = self.revision {
            args.push(OsString::from(format!("--revision={revision}")));
        }
    }

    fn list_path(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        let mut limits = self.limits;
        limits.max_entry_bytes = limits.max_entry_bytes.min(MAX_LISTING_BYTES);
        limits.max_archive_bytes = limits.max_archive_bytes.min(MAX_LISTING_BYTES);
        limits.memory_spill_bytes = limits.memory_spill_bytes.min(limits.max_entry_bytes);
        let output = self.runner.run(
            &self.list_args(path),
            cancel,
            self.timeout,
            &limits,
            limits.max_entry_bytes,
        )?;
        let mut bytes = Vec::new();
        let mut output = output;
        output.read_to_end(&mut bytes)?;
        let listed = parse_listing(&bytes)?;
        let raw_parent = self.raw_components(path);
        let placed = place(path, &raw_parent, listed)?;
        let entries = placed.entries;
        {
            let mut raw_paths = self
                .raw_paths
                .lock()
                .map_err(|_| VfsError::protocol("the Subversion path map was poisoned"))?;
            raw_paths.retain(|known, _| !known.starts_with(path) || known == path);
            raw_paths.extend(placed.raw_paths);
        }
        self.listings
            .lock()
            .map_err(|_| VfsError::protocol("the Subversion listing cache was poisoned"))?
            .insert(path.clone(), entries.clone());
        Ok(entries)
    }

    fn raw_components(&self, path: &VfsPath) -> Vec<String> {
        self.raw_paths
            .lock()
            .ok()
            .and_then(|paths| paths.get(path).cloned())
            .unwrap_or_else(|| path.components().map(str::to_owned).collect())
    }

    fn cached_listing(&self, dir: &VfsPath) -> VfsResult<Option<Vec<VfsEntry>>> {
        self.listings
            .lock()
            .map_err(|_| VfsError::protocol("the Subversion listing cache was poisoned"))
            .map(|listings| listings.get(dir).cloned())
    }

    fn metadata_with_cancel(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<VfsEntry> {
        if path.is_root() {
            return Ok(VfsEntry::directory(path.clone()));
        }
        cancel.check()?;
        let parent = path.parent().unwrap_or_else(VfsPath::root);
        let entries = match self.cached_listing(&parent)? {
            Some(entries) => entries,
            None => self.list_path(&parent, cancel)?,
        };
        entries
            .into_iter()
            .find(|entry| entry.path == *path)
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })
    }
}

impl FileSystem for SubversionFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: false,
            supports_timestamps: true,
            supports_attributes: false,
            stored_crc: false,
            random_access: true,
            content_available: true,
        }
    }

    fn root_label(&self) -> String {
        self.base_url.clone()
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        self.list_path(dir, cancel)
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        self.metadata_with_cancel(path, &Cancel::new())
    }

    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile> {
        cancel.check()?;
        let entry = self.metadata_with_cancel(path, cancel)?;
        if entry.kind == EntryKind::Directory {
            return Err(VfsError::IsADirectory { path: path.clone() });
        }
        let mut limits = self.limits;
        limits.memory_spill_bytes = limits.memory_spill_bytes.min(limits.max_entry_bytes);
        let output = self.runner.run(
            &self.cat_args(path),
            cancel,
            self.timeout,
            &limits,
            limits.max_entry_bytes,
        )?;
        // The command has already completed into the bounded spill buffer.
        // This length is measured from the bytes emitted by the svn client.
        let actual_size = output.len_hint().unwrap_or(0);
        if entry.size_is_exact && actual_size != entry.size {
            return Err(VfsError::protocol(format!(
                "the Subversion listing says {path} is {} bytes, but svn cat returned {actual_size} bytes",
                entry.size
            )));
        }
        if actual_size > limits.max_entry_bytes {
            return Err(VfsError::LimitExceeded {
                kind: crate::error::LimitKind::EntrySize,
                limit: limits.max_entry_bytes,
            });
        }
        Ok(output)
    }
}

trait CommandRunner: Send + Sync {
    fn run(
        &self,
        args: &[OsString],
        cancel: &Cancel,
        timeout: Duration,
        limits: &Limits,
        max_bytes: u64,
    ) -> VfsResult<OpenFile>;
}

struct ProcessRunner {
    program: OsString,
}

impl Default for ProcessRunner {
    fn default() -> Self {
        Self {
            program: OsString::from("svn"),
        }
    }
}

impl ProcessRunner {
    #[cfg(test)]
    fn for_program(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl CommandRunner for ProcessRunner {
    #[allow(
        clippy::too_many_lines,
        reason = "the child group, both output pipes and bounded materializer share one timeout lifecycle"
    )]
    fn run(
        &self,
        args: &[OsString],
        cancel: &Cancel,
        timeout: Duration,
        limits: &Limits,
        max_bytes: u64,
    ) -> VfsResult<OpenFile> {
        cancel.check()?;
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.group_spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                VfsError::unsupported(
                    "the Subversion command-line client (svn) is not installed or is not on PATH",
                )
            } else {
                VfsError::Io(error)
            }
        })?;
        let started_at = Instant::now();
        let child = Arc::new(Mutex::new(child));
        let (stdout, stderr) = {
            let mut locked = child
                .lock()
                .map_err(|_| VfsError::protocol("the svn process lock was poisoned"))?;
            let inner = locked.inner();
            let stdout = inner
                .stdout
                .take()
                .ok_or_else(|| VfsError::protocol("svn did not provide its output pipe"))?;
            let stderr = inner
                .stderr
                .take()
                .ok_or_else(|| VfsError::protocol("svn did not provide its error pipe"))?;
            (stdout, stderr)
        };
        let last_activity = Arc::new(Mutex::new(Instant::now()));
        let diagnostic_activity = Arc::clone(&last_activity);
        let stderr_closed = Arc::new(AtomicBool::new(false));
        let diagnostic_closed = Arc::clone(&stderr_closed);
        let diagnostic = thread::spawn(move || {
            read_diagnostic(ActivityReader {
                inner: stderr,
                last_activity: diagnostic_activity,
                closed: diagnostic_closed,
            })
        });
        let monitor_child = Arc::clone(&child);
        let monitor_cancel = cancel.clone();
        let monitor_activity = Arc::clone(&last_activity);
        let total_timeout = Duration::from_secs(limits.max_helper_seconds);
        let stdout_closed = Arc::new(AtomicBool::new(false));
        let monitor_stdout_closed = Arc::clone(&stdout_closed);
        let monitor_stderr_closed = Arc::clone(&stderr_closed);
        let reader_stdout_closed = Arc::clone(&stdout_closed);
        let monitor = thread::spawn(move || {
            let mut status = None;
            loop {
                if monitor_cancel.is_cancelled() {
                    stop_child(&monitor_child);
                    return Err(VfsError::Cancelled);
                }
                if started_at.elapsed() >= total_timeout {
                    stop_child(&monitor_child);
                    return Err(VfsError::Timeout {
                        operation: "the Subversion helper process".to_owned(),
                    });
                }
                let idle_for = monitor_activity
                    .lock()
                    .map(|last| last.elapsed())
                    .unwrap_or(timeout);
                if idle_for >= timeout {
                    stop_child(&monitor_child);
                    return Err(VfsError::Timeout {
                        operation: "the Subversion client".to_owned(),
                    });
                }
                if status.is_none() {
                    let mut process = monitor_child
                        .lock()
                        .map_err(|_| VfsError::protocol("the svn process lock was poisoned"))?;
                    status = process.try_wait()?;
                }
                if let Some(status) = status {
                    if monitor_stdout_closed.load(Ordering::SeqCst)
                        && monitor_stderr_closed.load(Ordering::SeqCst)
                    {
                        return Ok(status);
                    }
                }
                thread::sleep(POLL_INTERVAL);
            }
        });

        let mut limits = *limits;
        limits.max_entry_bytes = limits.max_entry_bytes.min(max_bytes);
        limits.max_archive_bytes = limits.max_archive_bytes.min(max_bytes);
        limits.memory_spill_bytes = limits.memory_spill_bytes.min(limits.max_entry_bytes);
        let result = materialize(
            ActivityReader {
                inner: stdout,
                last_activity,
                closed: reader_stdout_closed,
            },
            0,
            &limits,
            &Budget::new(limits.max_archive_bytes),
            cancel,
        );
        if result.is_err() {
            stop_child(&child);
            stdout_closed.store(true, Ordering::SeqCst);
            stderr_closed.store(true, Ordering::SeqCst);
        }
        let monitor_result = monitor.join();
        let diagnostic_result = diagnostic.join();
        let output = result?;
        let status = monitor_result
            .map_err(|_| VfsError::protocol("the svn process monitor stopped unexpectedly"))??;
        let diagnostic = diagnostic_result
            .map_err(|_| VfsError::protocol("the svn diagnostic reader stopped unexpectedly"))?;
        if !status.success() {
            return Err(client_error(&diagnostic));
        }
        Ok(output)
    }
}

fn stop_child(child: &Arc<Mutex<GroupChild>>) {
    if let Ok(mut process) = child.lock() {
        let _ = process.kill();
        let _ = process.wait();
    }
}

struct ActivityReader<R> {
    inner: R,
    last_activity: Arc<Mutex<Instant>>,
    closed: Arc<AtomicBool>,
}

impl<R: Read> Read for ActivityReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.read(buffer) {
            Ok(read) => {
                if read == 0 {
                    self.closed.store(true, Ordering::SeqCst);
                } else if let Ok(mut last_activity) = self.last_activity.lock() {
                    *last_activity = Instant::now();
                }
                Ok(read)
            }
            Err(error) => {
                self.closed.store(true, Ordering::SeqCst);
                Err(error)
            }
        }
    }
}

fn read_diagnostic(mut stderr: impl Read) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => return kept,
            Ok(read) => {
                let remaining = MAX_DIAGNOSTIC_BYTES.saturating_sub(kept.len());
                let keep = read.min(remaining);
                kept.extend_from_slice(chunk.get(..keep).unwrap_or_default());
            }
        }
    }
}

fn client_error(diagnostic: &[u8]) -> VfsError {
    let detail = String::from_utf8_lossy(diagnostic);
    let detail = detail.trim();
    if detail
        .to_ascii_lowercase()
        .contains("authentication failed")
        || detail.to_ascii_lowercase().contains("authorization failed")
    {
        return VfsError::auth_failed(short_diagnostic(detail));
    }
    VfsError::protocol(if detail.is_empty() {
        "the Subversion client did not complete the request".to_owned()
    } else {
        short_diagnostic(detail)
    })
}

fn short_diagnostic(detail: &str) -> String {
    detail.chars().take(240).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListedKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListedEntry {
    name: String,
    kind: ListedKind,
    size: u64,
    size_is_exact: bool,
    modified: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
enum Field {
    Name,
    Size,
    Date,
}

#[allow(
    clippy::too_many_lines,
    reason = "the XML event state machine keeps element context explicit in one pass"
)]
fn parse_listing(bytes: &[u8]) -> VfsResult<Vec<ListedEntry>> {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut current: Option<ListedEntry> = None;
    let mut field = None;
    let mut value = String::new();
    let mut out = Vec::new();
    let mut saw_lists = false;
    let mut saw_list = false;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(start)) => match start.name().as_ref() {
                b"lists" => saw_lists = true,
                b"list" => saw_list = true,
                b"entry" => {
                    if current.is_some() {
                        return Err(VfsError::protocol(
                            "the Subversion listing nests an entry inside another entry",
                        ));
                    }
                    let mut kind = None;
                    for attribute in start.attributes() {
                        let attribute = attribute.map_err(|error| {
                            VfsError::protocol(format!(
                                "the Subversion listing has an invalid entry: {error}"
                            ))
                        })?;
                        if attribute.key.as_ref() == b"kind" {
                            kind = match attribute.value.as_ref() {
                                b"file" => Some(ListedKind::File),
                                b"dir" => Some(ListedKind::Directory),
                                _ => None,
                            };
                        }
                    }
                    let kind = kind.ok_or_else(|| {
                        VfsError::protocol(
                            "the Subversion listing has an entry with an unknown kind",
                        )
                    })?;
                    current = Some(ListedEntry {
                        name: String::new(),
                        kind,
                        size: 0,
                        size_is_exact: kind == ListedKind::Directory,
                        modified: None,
                    });
                }
                b"name" if current.is_some() => {
                    field = Some(Field::Name);
                    value.clear();
                }
                b"size" if current.is_some() => {
                    field = Some(Field::Size);
                    value.clear();
                }
                b"date" if current.is_some() => {
                    field = Some(Field::Date);
                    value.clear();
                }
                _ => {}
            },
            Ok(Event::Text(text)) if field.is_some() => {
                let decoded = reader.decoder().decode(text.as_ref()).map_err(|error| {
                    VfsError::protocol(format!(
                        "the Subversion listing contains invalid text: {error}"
                    ))
                })?;
                let unescaped = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    VfsError::protocol(format!(
                        "the Subversion listing contains an invalid entity: {error}"
                    ))
                })?;
                value.push_str(&unescaped);
            }
            Ok(Event::GeneralRef(reference)) if field.is_some() => {
                let decoded = reference.decode().map_err(|error| {
                    VfsError::protocol(format!(
                        "the Subversion listing contains an invalid entity: {error}"
                    ))
                })?;
                if let Some(character) = reference.resolve_char_ref().map_err(|error| {
                    VfsError::protocol(format!(
                        "the Subversion listing contains an invalid character reference: {error}"
                    ))
                })? {
                    value.push(character);
                } else if let Some(replacement) =
                    quick_xml::escape::resolve_predefined_entity(&decoded)
                {
                    value.push_str(replacement);
                } else {
                    return Err(VfsError::protocol(format!(
                        "the Subversion listing uses an unknown entity: {}",
                        escaped(&decoded)
                    )));
                }
            }
            Ok(Event::CData(text)) if field.is_some() => {
                let decoded = reader.decoder().decode(text.as_ref()).map_err(|error| {
                    VfsError::protocol(format!(
                        "the Subversion listing contains invalid text: {error}"
                    ))
                })?;
                value.push_str(&decoded);
            }
            Ok(Event::End(end)) => match end.name().as_ref() {
                b"name" if matches!(field, Some(Field::Name)) => {
                    if let Some(entry) = current.as_mut() {
                        entry.name.clone_from(&value);
                    }
                    field = None;
                }
                b"size" if matches!(field, Some(Field::Size)) => {
                    if let Some(entry) = current.as_mut() {
                        entry.size = value.trim().parse().map_err(|_| {
                            VfsError::protocol("the Subversion listing has an invalid file size")
                        })?;
                        entry.size_is_exact = entry.kind == ListedKind::Directory;
                    }
                    field = None;
                }
                b"date" if matches!(field, Some(Field::Date)) => {
                    if let Some(entry) = current.as_mut() {
                        entry.modified = crate::remote::timestamp::parse_iso8601_utc(value.trim());
                    }
                    field = None;
                }
                b"entry" => {
                    let entry = current.take().ok_or_else(|| {
                        VfsError::protocol("the Subversion listing closes an unopened entry")
                    })?;
                    if entry.name.is_empty() {
                        return Err(VfsError::protocol(
                            "the Subversion listing has an entry with no name",
                        ));
                    }
                    out.push(entry);
                    if out.len() > MAX_LISTED {
                        return Err(VfsError::protocol(format!(
                            "the Subversion listing names more than {MAX_LISTED} entries"
                        )));
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(empty)) if empty.name().as_ref() == b"entry" => {
                return Err(VfsError::protocol(
                    "the Subversion listing has an empty entry without a name",
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(VfsError::protocol(format!(
                    "the Subversion listing is not usable XML: {error}"
                )))
            }
        }
        buffer.clear();
    }
    if !saw_lists || !saw_list || current.is_some() {
        return Err(VfsError::protocol(
            "the Subversion client returned an incomplete listing",
        ));
    }
    Ok(out)
}

struct PlacedListing {
    entries: Vec<VfsEntry>,
    raw_paths: BTreeMap<VfsPath, Vec<String>>,
}

fn place(
    dir: &VfsPath,
    raw_parent: &[String],
    listed: Vec<ListedEntry>,
) -> VfsResult<PlacedListing> {
    let mut entries = Vec::with_capacity(listed.len());
    let mut raw_paths = BTreeMap::new();
    let mut taken = crate::remote::ListedNames::default();
    for item in listed {
        let mut reason = None;
        let cleaned = sanitize(&item.name);
        if cleaned != item.name {
            reason = Some(format!(
                "the Subversion listing names an unusable path component: {}",
                escaped(&item.name)
            ));
        }
        let (name, repeated) = taken.claim(cleaned);
        if repeated {
            reason.get_or_insert_with(|| {
                format!(
                    "the Subversion listing names {} twice, or twice with the same letters in a different case",
                    escaped(&item.name)
                )
            });
        }
        let path = crate::remote::child_path(dir, &name)?;
        let mut raw_path = raw_parent.to_owned();
        raw_path.push(item.name.clone());
        raw_paths.insert(path.clone(), raw_path);
        entries.push(VfsEntry {
            path,
            name,
            kind: match item.kind {
                ListedKind::File => EntryKind::File,
                ListedKind::Directory => EntryKind::Directory,
            },
            size: item.size,
            size_is_exact: item.size_is_exact,
            modified: item.modified.map(crate::remote::timestamp::system_time),
            time_fidelity: TimeFidelity::Utc,
            created: None,
            attributes: None,
            crc32: None,
            link: None,
            version_info: None,
            refused: reason.is_some(),
            error: reason,
        });
    }
    Ok(PlacedListing { entries, raw_paths })
}

fn sanitize(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|character| {
            if character == '/' || character == '\\' || character == ':' || character.is_control() {
                '_'
            } else {
                character
            }
        })
        .take(255)
        .collect();
    if name.ends_with(['.', ' ']) {
        name.pop();
        name.push('_');
    }
    if name.is_empty() || name == "." || name == ".." {
        "unnamed".clone_into(&mut name);
    }
    super::local_display_name(name)
}

fn escaped(name: &str) -> String {
    format!("{:?}", name.chars().take(120).collect::<String>())
}

fn validate_url(url: &str) -> VfsResult<()> {
    if url.is_empty() || url.chars().any(char::is_control) || url.contains(['?', '#']) {
        return Err(VfsError::protocol(
            "the Subversion profile URL is empty or contains unsupported characters",
        ));
    }
    let (scheme, rest) = url.split_once("://").ok_or_else(|| {
        VfsError::protocol("the Subversion profile URL must name a repository scheme")
    })?;
    if !["file", "http", "https", "svn", "svn+ssh"]
        .iter()
        .any(|known| scheme.eq_ignore_ascii_case(known))
    {
        return Err(VfsError::unsupported(format!(
            "the Subversion client does not support the {} URL scheme",
            escaped(scheme)
        )));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(VfsError::protocol(
            "credentials in a Subversion URL are not allowed; use the profile username and the svn client's credential provider",
        ));
    }
    Ok(())
}

fn encode_component(component: &str) -> String {
    let mut encoded = String::with_capacity(component.len());
    for byte in component.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn trim_trailing_slashes(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    if scheme.eq_ignore_ascii_case("file") && rest.bytes().all(|byte| byte == b'/') {
        return "file:///".to_owned();
    }
    format!("{}://{}", scheme, rest.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    /// A displayed remote name cannot resolve to a local Windows device.
    #[cfg(windows)]
    #[test]
    fn remote_device_names_are_mapped_before_a_local_path_is_built() {
        for (raw, displayed) in [
            ("CON", "CON_"),
            ("nul.txt", "nul_.txt"),
            ("AuX .json", "AuX _.json"),
            ("COM1", "COM1_"),
            ("LPT².xml", "LPT²_.xml"),
        ] {
            assert_eq!(sanitize(raw), displayed, "{raw}");
            assert!(crate::stored::platform_refusal(std::ffi::OsStr::new(displayed)).is_none());
        }
        assert_eq!(sanitize("console.txt"), "console.txt");
    }

    use super::*;
    use std::collections::VecDeque;
    use std::io::Cursor;

    #[derive(Default)]
    struct FakeRunner {
        responses: Mutex<VecDeque<Vec<u8>>>,
        commands: Mutex<Vec<Vec<OsString>>>,
    }

    impl FakeRunner {
        fn with_responses(responses: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                commands: Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            args: &[OsString],
            cancel: &Cancel,
            _timeout: Duration,
            _limits: &Limits,
            _max_bytes: u64,
        ) -> VfsResult<OpenFile> {
            cancel.check()?;
            self.commands.lock().unwrap().push(args.to_vec());
            let bytes = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| VfsError::protocol("the test runner has no response"))?;
            let len = u64::try_from(bytes.len()).unwrap();
            Ok(OpenFile::seekable(Cursor::new(bytes), Some(len)))
        }
    }

    fn profile() -> SubversionProfile {
        SubversionProfile {
            url: "https://svn.example.test/project".to_owned(),
            revision: Some(42),
            username: "build-user".to_owned(),
            ..SubversionProfile::default()
        }
    }

    fn listing() -> Vec<u8> {
        br#"<?xml version="1.0"?>
<lists><list path="https://svn.example.test/project">
 <entry kind="file"><size>7</size><commit revision="1"><date>2024-01-02T03:04:05.000Z</date></commit><name>readme&amp;notes.txt</name></entry>
 <entry kind="dir"><commit revision="2"><date>2024-01-03T03:04:05.000Z</date></commit><name>src</name></entry>
</list></lists>"#
            .to_vec()
    }

    #[test]
    fn parses_xml_names_kinds_sizes_and_timestamps() {
        let entries = parse_listing(&listing()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "readme&notes.txt");
        assert_eq!(entries[0].kind, ListedKind::File);
        assert_eq!(entries[0].size, 7);
        assert!(!entries[0].size_is_exact);
        assert_eq!(
            entries[0].modified,
            Some(crate::remote::timestamp::civil_to_unix(2024, 1, 2, 3, 4, 5))
        );
        assert_eq!(entries[1].kind, ListedKind::Directory);
        assert!(entries[1].size_is_exact);
    }

    #[test]
    fn xml_names_keep_whitespace_around_entities() {
        let raw = br#"<lists><list><entry kind="file"><size> 1 </size><commit><date> 2024-01-02T03:04:05.000Z </date></commit><name>a &amp; b.txt</name></entry><entry kind="file"><size>2</size><name> lead.txt</name></entry><entry kind="file"><size>3</size><name>trail.txt </name></entry></list></lists>"#;
        let entries = parse_listing(raw).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["a & b.txt", " lead.txt", "trail.txt "]
        );
        assert_eq!(entries[0].size, 1);
        assert_eq!(
            entries[0].modified,
            Some(crate::remote::timestamp::civil_to_unix(2024, 1, 2, 3, 4, 5))
        );
    }

    #[test]
    fn a_folder_listing_is_read_only_and_uses_revision_and_encoded_child_paths() {
        let runner = Arc::new(FakeRunner::with_responses([listing()]));
        let fs = SubversionFs::with_runner(&profile(), &RemoteContext::default(), runner.clone())
            .unwrap();
        let rows = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path.as_str(), "readme&notes.txt");
        assert!(!fs.capabilities().writable);
        assert!(fs.capabilities().content_available);
        let commands = runner.commands.lock().unwrap();
        let text: Vec<String> = commands[0]
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
        assert!(text.contains(&"--non-interactive".to_owned()));
        assert!(text.contains(&"--no-auth-cache".to_owned()));
        assert!(text.contains(&"--username=build-user".to_owned()));
        assert!(text.contains(&"--revision=42".to_owned()));
    }

    #[test]
    fn a_file_reads_from_the_selected_revision_and_checks_the_listed_size() {
        let runner = Arc::new(FakeRunner::with_responses([listing(), b"payload".to_vec()]));
        let fs = SubversionFs::with_runner(&profile(), &RemoteContext::default(), runner.clone())
            .unwrap();
        let path = VfsPath::parse("readme&notes.txt").unwrap();
        let mut opened = fs.open(&path, &Cancel::new()).unwrap();
        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"payload");
        let commands = runner.commands.lock().unwrap();
        let cat: Vec<String> = commands[1]
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
        assert!(cat.contains(&"cat".to_owned()));
        assert!(cat.contains(&"--revision=42".to_owned()));
        assert!(cat.iter().any(|part| part.ends_with("readme%26notes.txt")));
    }

    #[test]
    fn a_displaced_row_reads_its_original_server_name() {
        let body = br#"<lists><list><entry kind="file"><size>1</size><name>A.txt</name></entry><entry kind="file"><size>1</size><name>a.txt</name></entry><entry kind="file"><size>1</size><name>a.txt~1</name></entry></list></lists>"#;
        let runner = Arc::new(FakeRunner::with_responses([body.to_vec(), b"a".to_vec()]));
        let fs = SubversionFs::with_runner(&profile(), &RemoteContext::default(), runner.clone())
            .unwrap();
        let rows = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
        assert_eq!(rows[1].path.as_str(), "a.txt~1");
        assert!(rows[1].refused, "a displaced path must not be compared");
        let mut opened = fs.open(&rows[1].path, &Cancel::new()).unwrap();
        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"a");
        let commands = runner.commands.lock().unwrap();
        let cat = commands
            .iter()
            .find(|args| args.iter().any(|arg| arg == "cat"))
            .unwrap();
        assert!(cat
            .iter()
            .any(|arg| arg.to_string_lossy().ends_with("/a.txt")));
    }

    #[test]
    fn opening_two_rows_from_one_listing_does_not_list_the_folder_again() {
        let body = br#"<lists><list><entry kind="file"><size>1</size><name>one</name></entry><entry kind="file"><size>1</size><name>two</name></entry></list></lists>"#;
        let runner = Arc::new(FakeRunner::with_responses([
            body.to_vec(),
            b"1".to_vec(),
            b"2".to_vec(),
        ]));
        let fs = SubversionFs::with_runner(&profile(), &RemoteContext::default(), runner.clone())
            .unwrap();
        let rows = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
        for row in rows {
            fs.open(&row.path, &Cancel::new()).unwrap();
        }
        let commands = runner.commands.lock().unwrap();
        assert_eq!(
            commands
                .iter()
                .filter(|args| args.iter().any(|arg| arg == "list"))
                .count(),
            1
        );
    }

    #[test]
    fn a_file_read_can_differ_from_the_untranslated_listing_size() {
        let runner = Arc::new(FakeRunner::with_responses([listing(), b"short".to_vec()]));
        let fs = SubversionFs::with_runner(&profile(), &RemoteContext::default(), runner.clone())
            .unwrap();
        let mut opened = fs
            .open(&VfsPath::parse("readme&notes.txt").unwrap(), &Cancel::new())
            .unwrap();
        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"short");
        let commands = runner.commands.lock().unwrap();
        assert!(!commands[1]
            .iter()
            .any(|argument| argument == "--ignore-keywords"));
    }

    #[test]
    fn names_that_cannot_be_paths_are_shown_as_refused_rows() {
        let raw = br#"<lists><list path="x"><entry kind="file"><size>1</size><name>../escape</name></entry></list></lists>"#;
        let rows = place(&VfsPath::root(), &[], parse_listing(raw).unwrap())
            .unwrap()
            .entries;
        assert_eq!(rows[0].name, ".._escape");
        assert!(rows[0].error.is_some());
        assert!(rows[0].refused);
        assert!(VfsPath::parse(rows[0].path.as_str()).is_ok());
    }

    #[test]
    fn malformed_and_incomplete_xml_are_refused() {
        for body in [b"<lists><list><entry".as_slice(), b"<root/>".as_slice()] {
            assert!(parse_listing(body).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_deadline_kills_a_descendant_holding_the_pipes_open() {
        let runner = ProcessRunner::for_program("/bin/sh");
        let started = Instant::now();
        let error = runner
            .run(
                &["-c".into(), "sleep 1 & exit 0".into()],
                &Cancel::new(),
                Duration::from_millis(120),
                &Limits::default(),
                1024,
            )
            .unwrap_err();

        assert!(matches!(error, VfsError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(unix)]
    #[test]
    fn process_cancel_kills_a_descendant_holding_the_pipes_open() {
        let runner = ProcessRunner::for_program("/bin/sh");
        let cancel = Cancel::new();
        let worker_cancel = cancel.clone();
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(120));
            worker_cancel.cancel();
        });
        let started = Instant::now();
        let error = runner
            .run(
                &["-c".into(), "sleep 1 & exit 0".into()],
                &cancel,
                Duration::from_secs(5),
                &Limits::default(),
                1024,
            )
            .unwrap_err();
        cancellation.join().unwrap();

        assert!(matches!(error, VfsError::Cancelled));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(windows)]
    #[test]
    fn process_deadline_kills_a_descendant_holding_the_pipes_open() {
        let runner = ProcessRunner::for_program("cmd.exe");
        let started = Instant::now();
        let error = runner
            .run(
                &[
                    "/C".into(),
                    "start /B ping 127.0.0.1 -n 15 & exit /B 0".into(),
                ],
                &Cancel::new(),
                Duration::from_millis(120),
                &Limits::default(),
                1024,
            )
            .unwrap_err();

        assert!(matches!(error, VfsError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[cfg(windows)]
    #[test]
    fn process_cancel_kills_a_descendant_holding_the_pipes_open() {
        let runner = ProcessRunner::for_program("powershell.exe");
        let cancel = Cancel::new();
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("child-ready");
        let script = format!(
            "$child = Start-Process powershell.exe -PassThru -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 15'; [IO.File]::WriteAllText('{}', [string]$child.Id); Start-Sleep -Seconds 15",
            marker.display(),
        );
        let worker_cancel = cancel.clone();
        let worker_marker = marker.clone();
        let cancellation = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !worker_marker.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let child_started = worker_marker.exists();
            worker_cancel.cancel();
            (child_started, Instant::now())
        });
        let error = runner
            .run(
                &[
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    script.into(),
                ],
                &cancel,
                Duration::from_secs(20),
                &Limits::default(),
                1024,
            )
            .unwrap_err();
        let (child_started, cancelled_at) = cancellation.join().unwrap();

        assert!(child_started, "the silent child did not start");
        assert!(matches!(error, VfsError::Cancelled));
        assert!(cancelled_at.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn process_timeout_is_refreshed_by_output_during_a_long_transfer() {
        let runner = ProcessRunner::for_program("/bin/sh");
        let mut output = runner
            .run(
                &[
                    "-c".into(),
                    "i=0; while [ $i -lt 8 ]; do printf x; sleep 1; i=$((i + 1)); done".into(),
                ],
                &Cancel::new(),
                Duration::from_secs(5),
                &Limits::default(),
                1024,
            )
            .unwrap();
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes.len(), 8);
    }

    #[cfg(unix)]
    #[test]
    fn process_total_helper_limit_still_applies_during_a_continuing_transfer() {
        let runner = ProcessRunner::for_program("/bin/sh");
        let limits = Limits {
            max_helper_seconds: 1,
            ..Limits::default()
        };
        let started = Instant::now();
        let error = runner
            .run(
                &[
                    "-c".into(),
                    "i=0; while [ $i -lt 30 ]; do printf x; sleep 0.06; i=$((i + 1)); done".into(),
                ],
                &Cancel::new(),
                Duration::from_millis(200),
                &limits,
                1024,
            )
            .unwrap_err();

        assert!(matches!(error, VfsError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_millis(1_500));
    }

    #[cfg(windows)]
    #[test]
    fn process_timeout_is_refreshed_by_output_during_a_long_transfer() {
        let runner = ProcessRunner::for_program("powershell.exe");
        let mut output = runner
            .run(
                &[
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "for ($i = 0; $i -lt 8; $i++) { [Console]::Out.Write('x'); [Console]::Out.Flush(); Start-Sleep -Seconds 1 }".into(),
                ],
                &Cancel::new(),
                Duration::from_secs(5),
                &Limits::default(),
                1024,
            )
            .unwrap();
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes.len(), 8);
    }

    #[cfg(windows)]
    #[test]
    fn process_total_helper_limit_still_applies_during_a_continuing_transfer() {
        let runner = ProcessRunner::for_program("powershell.exe");
        let limits = Limits {
            max_helper_seconds: 1,
            ..Limits::default()
        };
        let started = Instant::now();
        let error = runner
            .run(
                &[
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "for ($i = 0; $i -lt 30; $i++) { [Console]::Out.Write('x'); [Console]::Out.Flush(); Start-Sleep -Milliseconds 60 }".into(),
                ],
                &Cancel::new(),
                Duration::from_secs(5),
                &limits,
                1024,
            )
            .unwrap_err();

        assert!(matches!(error, VfsError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[cfg(unix)]
    #[test]
    fn process_output_limit_is_reported_without_waiting_for_idle_timeout() {
        let runner = ProcessRunner::for_program("/bin/sh");
        let idle_timeout = Duration::from_secs(5);
        let started = Instant::now();
        let error = runner
            .run(
                &["-c".into(), "head -c 1048576 /dev/zero".into()],
                &Cancel::new(),
                idle_timeout,
                &Limits::default(),
                1024,
            )
            .unwrap_err();

        assert!(matches!(error, VfsError::LimitExceeded { .. }));
        assert!(started.elapsed() < idle_timeout);
    }

    #[cfg(windows)]
    #[test]
    fn process_output_limit_is_reported_without_waiting_for_idle_timeout() {
        let runner = ProcessRunner::for_program("powershell.exe");
        let idle_timeout = Duration::from_secs(5);
        let started = Instant::now();
        let error = runner
            .run(
                &[
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "$bytes = [byte[]]::new(1048576); [Console]::OpenStandardOutput().Write($bytes, 0, $bytes.Length)".into(),
                ],
                &Cancel::new(),
                idle_timeout,
                &Limits::default(),
                1024,
            )
            .unwrap_err();

        assert!(matches!(error, VfsError::LimitExceeded { .. }));
        assert!(started.elapsed() < idle_timeout);
    }

    #[test]
    fn url_credentials_and_profile_passwords_are_not_passed_to_svn() {
        for url in [
            "https://user:pass@svn.example.test/repo",
            "https://user@svn.example.test/repo",
            "https://svn.example.test/repo?token=secret",
        ] {
            let mut configured = profile();
            configured.url = url.to_owned();
            assert!(SubversionFs::with_runner(
                &configured,
                &RemoteContext::default(),
                Arc::new(FakeRunner::default())
            )
            .is_err());
        }
        let mut configured = profile();
        configured.password = crate::remote::SecretRef::new("svn/password");
        let error = SubversionFs::with_runner(
            &configured,
            &RemoteContext::default(),
            Arc::new(FakeRunner::default()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not passed"));
    }

    #[test]
    fn url_paths_are_encoded_component_by_component() {
        assert_eq!(encode_component("a b&c"), "a%20b%26c");
        let fs = SubversionFs::with_runner(
            &profile(),
            &RemoteContext::default(),
            Arc::new(FakeRunner::default()),
        )
        .unwrap();
        assert_eq!(
            fs.url(&VfsPath::parse("a b/x&y").unwrap()),
            "https://svn.example.test/project/a%20b/x%26y"
        );
    }

    #[test]
    fn a_root_file_url_keeps_its_three_slashes() {
        assert_eq!(trim_trailing_slashes("file:///"), "file:///");
        assert_eq!(trim_trailing_slashes("file:///repo/"), "file:///repo");
    }
}
