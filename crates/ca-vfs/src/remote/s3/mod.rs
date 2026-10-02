//! An object store reached with the S3 interface.
//!
//! An object store has no folders. A listing asks for the keys under one
//! prefix with a delimiter, and the prefixes the server groups are shown as
//! folders. A folder that holds nothing therefore exists only as the empty
//! object whose key ends in a separator, which is what this implementation
//! writes when a folder is created.
//!
//! The store states a time to the second in UTC and a size as an exact byte
//! count, so both are exact.
//!
//! A write is one request and the store replaces an object only when the
//! whole request arrives, so an interrupted upload leaves the previous
//! content in place without a temporary name. A rename is a copy followed by
//! a delete, which is the only move the interface has.

pub mod sigv4;

use std::io::Read;
use std::time::Duration;

use quick_xml::events::Event;

use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsEntry};
use crate::error::{VfsError, VfsResult};
use crate::fs::{Capabilities, FileSystem, OpenFile};
use crate::limits::{Budget, LimitedReader};
use crate::path::VfsPath;
use crate::remote::http::{HttpClient, RequestBody, Response, Url};
use crate::remote::profile::{S3Auth, S3Profile};
use crate::remote::secret::Secret;
use crate::remote::{child_path, RemoteContext};

/// How many keys one listing request asks for.
const PAGE_SIZE: usize = 1000;

/// How many listing pages one folder may take before the server is treated as
/// endless.
const MAX_PAGES: usize = 10_000;

/// How many entries one folder listing may gather across its pages. The page
/// ceiling alone still lets a store return millions of keys, which would cost
/// the caller's memory rather than a bounded amount.
const MAX_LISTED: usize = 1_000_000;

/// A file system backed by an object store.
pub struct S3Fs {
    client: HttpClient,
    endpoint: Url,
    bucket: String,
    region: String,
    path_style: bool,
    credentials: Option<sigv4::Credentials>,
    context: RemoteContext,
}

impl std::fmt::Debug for S3Fs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Fs")
            .field("endpoint", &self.endpoint.origin())
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl S3Fs {
    /// Open the bucket a profile names.
    ///
    /// # Errors
    /// Returns [`VfsError::Protocol`] when the profile names no bucket or the
    /// address does not parse, and whatever the first request reports.
    pub fn connect(
        settings: &S3Profile,
        context: &RemoteContext,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        if settings.bucket.trim().is_empty() {
            return Err(VfsError::protocol("the profile names no bucket".to_owned()));
        }
        let region = if settings.region.trim().is_empty() {
            "us-east-1".to_owned()
        } else {
            settings.region.trim().to_owned()
        };
        let endpoint = if settings.endpoint.trim().is_empty() {
            Url::parse(&format!("https://s3.{region}.amazonaws.com"))?
        } else {
            Url::parse(&settings.endpoint)?
        };
        let credentials = resolve_credentials(&settings.auth, context)?;
        let timeout = settings
            .timeout_seconds
            .map_or(context.call_timeout, |value| {
                Duration::from_secs(u64::from(value))
            });
        let tls = crate::remote::tls::TlsOptions::try_from(&settings.tls)?;
        let client = HttpClient::new(&tls, None, timeout)?;
        let fs = Self {
            client,
            endpoint,
            bucket: settings.bucket.trim().to_owned(),
            region,
            path_style: settings.path_style,
            credentials,
            context: context.clone(),
        };
        // One listing proves the address, the credentials and the bucket.
        fs.page("", "/", None, cancel)?;
        Ok(fs)
    }
}

/// The request path of one key, with the bucket in it when the address carries
/// the bucket in the path rather than in the host name.
///
/// Every separator of the key is kept. Two separators in a row name a key with
/// an empty component, which the store holds and addresses like any other, so
/// collapsing them would make such a key unreachable.
fn canonical_key_path(bucket: Option<&str>, key: &str) -> String {
    let encoded = sigv4::encode_path(key);
    match bucket {
        Some(bucket) => format!("/{bucket}/{encoded}"),
        None => format!("/{encoded}"),
    }
}

impl S3Fs {
    /// The address one request goes to, and the host header it carries.
    fn target(&self, key: &str, query: &[(String, String)]) -> (Url, String, String) {
        let (path, host) = if self.path_style {
            (
                canonical_key_path(Some(&self.bucket), key),
                self.endpoint.host_header(),
            )
        } else {
            (
                canonical_key_path(None, key),
                format!("{}.{}", self.bucket, self.endpoint.host_header()),
            )
        };
        let query_text = sigv4::canonical_query(query);
        let full = if query_text.is_empty() {
            path.clone()
        } else {
            format!("{path}?{query_text}")
        };
        (self.endpoint.with_path(&full), host, path)
    }

    /// Send one signed request.
    fn send(
        &self,
        method: &str,
        key: &str,
        query: &[(String, String)],
        headers: &[(String, String)],
        payload: Vec<u8>,
        cancel: &Cancel,
    ) -> VfsResult<Response> {
        let (url, host, canonical_path) = self.target(key, query);
        let now = crate::remote::timestamp::unix_seconds(std::time::SystemTime::now());
        let mut signed = match &self.credentials {
            Some(credentials) => sigv4::sign(
                &sigv4::Request {
                    method,
                    path: &canonical_path,
                    query,
                    headers,
                    payload: &payload,
                    host: &host,
                    region: &self.region,
                    service: "s3",
                    now,
                },
                credentials,
            ),
            None => headers.to_vec(),
        };
        // The client writes its own host header from the address, and the one
        // the signature covers must be the same, so the bucket in the host
        // name reaches the socket through the address instead.
        signed.retain(|(name, _)| name != "host");
        let url = if self.path_style {
            url
        } else {
            Url {
                host: host.split(':').next().unwrap_or(&host).to_owned(),
                ..url
            }
        };
        let body = if payload.is_empty() {
            RequestBody::Empty
        } else {
            RequestBody::Bytes(payload)
        };
        self.client.send(method, &url, &signed, body, cancel)
    }

    /// The key one path maps to.
    fn key(path: &VfsPath) -> String {
        path.as_str().to_owned()
    }

    /// One page of a listing.
    fn page(
        &self,
        prefix: &str,
        delimiter: &str,
        token: Option<&str>,
        cancel: &Cancel,
    ) -> VfsResult<Listing> {
        let mut query = vec![
            ("list-type".to_owned(), "2".to_owned()),
            ("max-keys".to_owned(), PAGE_SIZE.to_string()),
        ];
        if !prefix.is_empty() {
            query.push(("prefix".to_owned(), prefix.to_owned()));
        }
        if !delimiter.is_empty() {
            query.push(("delimiter".to_owned(), delimiter.to_owned()));
        }
        if let Some(value) = token {
            query.push(("continuation-token".to_owned(), value.to_owned()));
        }
        let mut response = self.send("GET", "", &query, &[], Vec::new(), cancel)?;
        check(&mut response, "the listing", &self.context.limits)?;
        let body = response.read_body(&self.context.limits, cancel)?;
        parse_listing(&body)
    }

    /// Open a read at `offset`.
    ///
    /// # Errors
    /// Returns whatever the request reports.
    pub fn open_at(&self, path: &VfsPath, offset: u64, cancel: &Cancel) -> VfsResult<OpenFile> {
        let mut headers = Vec::new();
        if offset > 0 {
            headers.push(("range".to_owned(), format!("bytes={offset}-")));
        }
        let mut response = self.send("GET", &Self::key(path), &[], &headers, Vec::new(), cancel)?;
        if response.status == 404 {
            return Err(VfsError::NotFound { path: path.clone() });
        }
        if offset > 0 && response.status != 206 {
            return Err(VfsError::unsupported(
                "the server does not restart a transfer at an offset",
            ));
        }
        check(&mut response, "the read", &self.context.limits)?;
        let length = response
            .header("content-length")
            .and_then(|value| value.trim().parse::<u64>().ok());
        let budget = Budget::new(self.context.limits.max_archive_bytes);
        let reader = LimitedReader::new(
            response.body,
            0,
            self.context.limits,
            budget,
            cancel.clone(),
        );
        Ok(OpenFile::streaming(reader, length))
    }

    /// Every key under `path`, for a delete of a whole folder.
    fn keys_under(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<String>> {
        let prefix = if path.is_root() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            cancel.check()?;
            let page = self.page(&prefix, "", token.as_deref(), cancel)?;
            for object in page.objects {
                out.push(object.key);
            }
            if out.len() > MAX_LISTED {
                return Err(VfsError::protocol(format!(
                    "the listing of {path} names more than {MAX_LISTED} entries"
                )));
            }
            match page.next {
                Some(next) if !next.is_empty() && Some(&next) != token.as_ref() => {
                    token = Some(next);
                }
                _ => return Ok(out),
            }
        }
        Err(VfsError::protocol(format!(
            "the listing of {path} ran past {MAX_PAGES} pages"
        )))
    }
}

/// One object in a listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Object {
    /// Key of the object.
    key: String,
    /// Size in bytes.
    size: u64,
    /// Modification time in seconds from the Unix epoch.
    modified: Option<i64>,
    /// Checksum the store recorded, where it is a plain content hash.
    etag: Option<String>,
}

/// One page of a listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Listing {
    /// Objects in this page.
    objects: Vec<Object>,
    /// Prefixes the server grouped, which are shown as folders.
    prefixes: Vec<String>,
    /// Token for the page that follows, where there is one.
    next: Option<String>,
}

/// Read a listing reply.
fn parse_listing(body: &[u8]) -> VfsResult<Listing> {
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut out = Listing::default();
    let mut current = Object::default();
    let mut in_contents = false;
    let mut in_prefix = false;
    let mut field: Option<&'static str> = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(start)) => match start.name().as_ref() {
                b"Contents" => {
                    in_contents = true;
                    current = Object::default();
                }
                b"CommonPrefixes" => in_prefix = true,
                b"Key" => field = Some("key"),
                b"Size" => field = Some("size"),
                b"LastModified" => field = Some("modified"),
                b"ETag" => field = Some("etag"),
                b"Prefix" if in_prefix => field = Some("prefix"),
                b"NextContinuationToken" => field = Some("next"),
                _ => {}
            },
            Ok(Event::Text(text)) => {
                let value = String::from_utf8_lossy(text.as_ref()).into_owned();
                match field {
                    Some("key") if in_contents => current.key = value,
                    Some("size") if in_contents => current.size = value.trim().parse().unwrap_or(0),
                    Some("modified") if in_contents => {
                        current.modified = crate::remote::timestamp::parse_iso8601_utc(&value);
                    }
                    Some("etag") if in_contents => {
                        current.etag = Some(value.trim_matches('"').to_owned());
                    }
                    Some("prefix") if in_prefix => out.prefixes.push(value),
                    Some("next") => out.next = Some(value),
                    _ => {}
                }
            }
            Ok(Event::End(end)) => {
                match end.name().as_ref() {
                    b"Contents" => {
                        in_contents = false;
                        if !current.key.is_empty() {
                            out.objects.push(std::mem::take(&mut current));
                        }
                    }
                    b"CommonPrefixes" => in_prefix = false,
                    _ => {}
                }
                field = None;
            }
            Ok(_) => {}
            Err(error) => {
                return Err(VfsError::protocol(format!(
                    "the listing is not usable XML: {error}"
                )))
            }
        }
        buffer.clear();
    }
    Ok(out)
}

/// True when a copy reply states that the copy finished.
///
/// An empty body is accepted because a store that answers a copy with no body
/// has nothing to contradict its status.
fn copy_succeeded(body: &[u8]) -> bool {
    if body.iter().all(u8::is_ascii_whitespace) {
        return true;
    }
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut completed = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(start)) => match start.name().as_ref() {
                b"Error" => return false,
                b"CopyObjectResult" | b"CopyPartResult" => completed = true,
                _ => {}
            },
            Ok(Event::Empty(empty)) => match empty.name().as_ref() {
                b"Error" => return false,
                b"CopyObjectResult" | b"CopyPartResult" => completed = true,
                _ => {}
            },
            Ok(_) => {}
            Err(_) => return false,
        }
        buffer.clear();
    }
    completed
}

/// The store's own code for a failure, where the body states one.
fn error_code(body: &[u8]) -> Option<String> {
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut in_code = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(start)) if start.name().as_ref() == b"Code" => in_code = true,
            Ok(Event::Text(text)) if in_code => {
                let value: String = String::from_utf8_lossy(text.as_ref())
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .take(64)
                    .collect();
                if !value.is_empty() {
                    return Some(value);
                }
                in_code = false;
            }
            Ok(Event::End(_)) => in_code = false,
            Ok(_) => {}
        }
        buffer.clear();
    }
    None
}

/// Fail on a status that does not name success.
///
/// The store states the reason in the body, so it is read and the code it
/// names is carried into the message. A signature refused for a clock that
/// disagrees with the store's own reads differently from a wrong key.
fn check(response: &mut Response, what: &str, limits: &crate::limits::Limits) -> VfsResult<()> {
    if response.is_success() {
        return Ok(());
    }
    let status = response.status;
    let code = response
        .read_body(limits, &Cancel::new())
        .ok()
        .and_then(|body| error_code(&body));
    let detail = match &code {
        Some(value) => format!("status {status}, {value}"),
        None => format!("status {status}"),
    };
    if code.as_deref() == Some("RequestTimeTooSkewed") {
        return Err(VfsError::protocol(format!(
            "{what} was refused because this machine's clock disagrees with the store's; set the \
             clock and try again"
        )));
    }
    match status {
        401 | 403 => Err(VfsError::auth_failed(format!(
            "{what} was refused with {detail}"
        ))),
        404 => Err(VfsError::protocol(format!("{what} found nothing"))),
        _ => Err(VfsError::protocol(format!("{what} failed with {detail}"))),
    }
}

/// Where the credentials of one profile come from.
fn resolve_credentials(
    auth: &S3Auth,
    context: &RemoteContext,
) -> VfsResult<Option<sigv4::Credentials>> {
    match auth {
        S3Auth::Anonymous { .. } => Ok(None),
        S3Auth::Saved {
            access_key_id,
            secret_access_key,
            session_token,
            unknown: _,
        } => Ok(Some(sigv4::Credentials {
            access_key_id: access_key_id.clone(),
            secret_access_key: context
                .secret(secret_access_key)
                .unwrap_or_else(|| Secret::new(String::new())),
            session_token: context.secret(session_token),
        })),
        S3Auth::Environment { .. } => {
            let id = std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_default();
            let key = std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default();
            if id.is_empty() || key.is_empty() {
                return Err(VfsError::auth_failed(
                    "the environment holds no access key".to_owned(),
                ));
            }
            Ok(Some(sigv4::Credentials {
                access_key_id: id,
                secret_access_key: Secret::new(key),
                session_token: std::env::var("AWS_SESSION_TOKEN").ok().map(Secret::new),
            }))
        }
        S3Auth::CredentialsFile { .. } => Err(VfsError::unsupported(
            "reading credentials from a file is not implemented; name the keys in the profile or \
             put them in the environment",
        )),
        S3Auth::Unknown(_) => Err(VfsError::unsupported(
            "the profile names a credential source this build does not know",
        )),
    }
}

impl FileSystem for S3Fs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: true,
            supports_timestamps: true,
            supports_attributes: false,
            stored_crc: false,
            random_access: false,
            content_available: true,
        }
    }

    fn root_label(&self) -> String {
        format!("s3://{}", self.bucket)
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        let prefix = if dir.is_root() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut out = Vec::new();
        let mut taken = crate::remote::ListedNames::default();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            cancel.check()?;
            let page = self.page(&prefix, "/", page_token.as_deref(), cancel)?;
            for folder in &page.prefixes {
                let raw = folder
                    .strip_prefix(&prefix)
                    .unwrap_or(folder)
                    .trim_end_matches('/');
                push(&mut out, &mut taken, dir, raw, None);
            }
            for object in &page.objects {
                let raw = object.key.strip_prefix(&prefix).unwrap_or(&object.key);
                if raw.is_empty() || raw.ends_with('/') {
                    // The empty object that stands for the folder itself.
                    continue;
                }
                push(&mut out, &mut taken, dir, raw, Some(object));
            }
            if out.len() > MAX_LISTED {
                return Err(VfsError::protocol(format!(
                    "the listing of {dir} names more than {MAX_LISTED} entries"
                )));
            }
            match page.next {
                // A token the store repeats, or an empty one, would turn the
                // listing into a loop.
                Some(next) if !next.is_empty() && Some(&next) != page_token.as_ref() => {
                    page_token = Some(next);
                }
                _ => return Ok(out),
            }
        }
        Err(VfsError::protocol(format!(
            "the listing of {dir} ran past {MAX_PAGES} pages"
        )))
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        if path.is_root() {
            return Ok(VfsEntry::directory(path.clone()));
        }
        let cancel = Cancel::new();
        let response = self.send("HEAD", &Self::key(path), &[], &[], Vec::new(), &cancel)?;
        if response.is_success() {
            let size = response
                .header("content-length")
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let modified = response
                .header("last-modified")
                .and_then(crate::remote::timestamp::parse_http_date);
            let mut entry = VfsEntry::file(path.clone(), size);
            entry.modified = modified.map(crate::remote::timestamp::system_time);
            return Ok(entry);
        }
        // A key that is not there may still be a prefix other keys sit under.
        let page = self.page(&format!("{path}/"), "/", None, &cancel)?;
        if page.objects.is_empty() && page.prefixes.is_empty() {
            return Err(VfsError::NotFound { path: path.clone() });
        }
        Ok(VfsEntry::directory(path.clone()))
    }

    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile> {
        self.open_at(path, 0, cancel)
    }

    fn create_dir(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        if path.is_root() {
            return Ok(());
        }
        // The client writes the framing header from the body it is given, so
        // naming one here would put two of them in the request.
        let mut response = self.send("PUT", &format!("{path}/"), &[], &[], Vec::new(), cancel)?;
        check(&mut response, "the folder marker", &self.context.limits)
    }

    fn write_file(&self, path: &VfsPath, content: &mut dyn Read, cancel: &Cancel) -> VfsResult<()> {
        let budget = Budget::new(self.context.limits.max_archive_bytes);
        let bytes = crate::limits::read_bounded(content, 0, &self.context.limits, &budget, cancel)?;
        let mut response = self.send("PUT", &Self::key(path), &[], &[], bytes, cancel)?;
        check(&mut response, "the upload", &self.context.limits)
    }

    fn delete(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        if path.is_root() {
            return Err(VfsError::unsupported(
                "the root of a profile is not deleted",
            ));
        }
        let entry = self.metadata(path)?;
        if entry.is_dir() {
            for key in self.keys_under(path, cancel)? {
                cancel.check()?;
                let mut response = self.send("DELETE", &key, &[], &[], Vec::new(), cancel)?;
                check(&mut response, "the delete", &self.context.limits)?;
            }
            // The folder marker may never have been written.
            let _ = self.send("DELETE", &format!("{path}/"), &[], &[], Vec::new(), cancel);
            return Ok(());
        }
        let mut response = self.send("DELETE", &Self::key(path), &[], &[], Vec::new(), cancel)?;
        check(&mut response, "the delete", &self.context.limits)
    }

    fn rename(&self, from: &VfsPath, to: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let source = format!("/{}/{}", self.bucket, sigv4::encode_path(from.as_str()));
        let mut response = self.send(
            "PUT",
            &Self::key(to),
            &[],
            &[("x-amz-copy-source".to_owned(), source)],
            Vec::new(),
            cancel,
        )?;
        check(&mut response, "the copy", &self.context.limits)?;
        // A copy answers with a success status and then reports the failure in
        // the body. Deleting the source on that status would lose the object.
        let body = response.read_body(&self.context.limits, cancel)?;
        if !copy_succeeded(&body) {
            return Err(VfsError::protocol(format!(
                "the copy did not complete: {}",
                error_code(&body).unwrap_or_else(|| "the store reported no result".to_owned())
            )));
        }
        let mut removed = self.send("DELETE", &Self::key(from), &[], &[], Vec::new(), cancel)?;
        check(
            &mut removed,
            "the delete after the copy",
            &self.context.limits,
        )
    }
}

/// Add one entry, keeping a name the server listed twice.
fn push(
    out: &mut Vec<VfsEntry>,
    taken: &mut crate::remote::ListedNames,
    dir: &VfsPath,
    raw: &str,
    object: Option<&Object>,
) {
    if raw.is_empty() {
        return;
    }
    let mut reason = None;
    let cleaned = sanitize(raw);
    if cleaned != raw {
        reason = Some(format!(
            "the store holds a key that no file system can hold as a name: {}",
            escape(raw)
        ));
    }
    let (name, repeated) = taken.claim(cleaned);
    if repeated {
        reason.get_or_insert_with(|| {
            format!(
                "the store holds {} twice, or twice with the same letters in a different case",
                escape(raw)
            )
        });
    }
    let Ok(path) = child_path(dir, &name) else {
        return;
    };
    out.push(VfsEntry {
        path,
        name,
        kind: if object.is_some() {
            EntryKind::File
        } else {
            EntryKind::Directory
        },
        size: object.map_or(0, |item| item.size),
        size_is_exact: true,
        modified: object
            .and_then(|item| item.modified)
            .map(crate::remote::timestamp::system_time),
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

/// Replace what no file system can hold in a key.
fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' || ch == ':' || ch == '\0' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .take(255)
        .collect();
    if out.ends_with('.') || out.ends_with(' ') {
        out.pop();
        out.push('_');
    }
    if out.is_empty() || out == "." || out == ".." {
        "unnamed".clone_into(&mut out);
    }
    super::local_display_name(out)
}

/// Render a key safely for an error message.
fn escape(name: &str) -> String {
    format!("{:?}", name.chars().take(120).collect::<String>())
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

    #[test]
    fn a_listing_reply_parses() {
        let body = br#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>bucket</Name>
  <Prefix></Prefix>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>report.txt</Key>
    <LastModified>2024-01-02T03:04:05.000Z</LastModified>
    <ETag>&quot;abc&quot;</ETag>
    <Size>1234</Size>
  </Contents>
  <CommonPrefixes><Prefix>sub/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
        let listing = parse_listing(body).unwrap();
        assert_eq!(listing.objects.len(), 1);
        let object = listing.objects.first().unwrap();
        assert_eq!(object.key, "report.txt");
        assert_eq!(object.size, 1234);
        assert_eq!(object.etag.as_deref(), Some("abc"));
        assert_eq!(
            object.modified,
            Some(crate::remote::timestamp::civil_to_unix(2024, 1, 2, 3, 4, 5))
        );
        assert_eq!(listing.prefixes, vec!["sub/".to_owned()]);
        assert!(listing.next.is_none());
    }

    #[test]
    fn a_key_keeps_every_separator_it_ends_with() {
        assert_eq!(canonical_key_path(Some("bucket"), "a//"), "/bucket/a//");
        assert_eq!(canonical_key_path(None, "a//"), "/a//");
        assert_eq!(canonical_key_path(Some("bucket"), ""), "/bucket/");
        assert_eq!(canonical_key_path(None, ""), "/");
        assert_eq!(canonical_key_path(None, "a//b"), "/a//b");
    }

    #[test]
    fn a_reply_that_is_not_xml_is_reported_rather_than_panicking() {
        assert!(parse_listing(b"<ListBucketResult><<<").is_err());
        assert_eq!(parse_listing(b"").unwrap(), Listing::default());
    }

    #[test]
    fn a_hostile_key_never_escapes_the_root() {
        let mut out = Vec::new();
        let mut taken = crate::remote::ListedNames::default();
        for key in ["../escape", "a/b", "..", "", "x\u{7}y"] {
            push(&mut out, &mut taken, &VfsPath::root(), key, None);
        }
        for entry in &out {
            assert!(VfsPath::parse(entry.path.as_str()).is_ok());
            assert_eq!(entry.path.depth(), 1);
        }
        assert!(out.iter().all(|entry| entry.error.is_some()));
    }
}
