//! A small HTTP/1.1 client for the share and object store protocols.
//!
//! The client is written against this crate's own socket wrapper so that every
//! request obeys the caller's deadline and cancellation flag, which a general
//! purpose client does not offer. It opens one connection per request, which
//! keeps the state machine small enough to reason about.
//!
//! Two rules bound where a request can end up:
//!
//! - redirects are followed at most [`MAX_REDIRECTS`] times;
//! - a secure request is never redirected to a plain one, so a server cannot
//!   move credentials or content onto an unprotected connection.

use std::io::{Read, Write};
use std::sync::Arc;

use base64::Engine;
use rustls::ClientConfig;

use crate::cancel::Cancel;
use crate::error::{VfsError, VfsResult};
use crate::remote::net::{connect, read_line, AddressPreference, Deadline, PollStream, Transport};
use crate::remote::profile::HttpAuthScheme;
use crate::remote::secret::Secret;

/// How many redirects one request follows.
pub const MAX_REDIRECTS: usize = 5;

/// Most bytes one header line may hold.
const MAX_HEADER_LINE: usize = 16 * 1024;

/// Most header lines one response may hold.
const MAX_HEADERS: usize = 200;

/// Most bytes one chunk header line may hold.
const MAX_CHUNK_LINE: usize = 64;

/// Largest chunk length a server may declare. A chunk is read in pieces, so
/// the ceiling only rejects a length no transfer can mean.
const MAX_CHUNK_BYTES: u64 = 1 << 40;

/// Maximum response body retained by a protocol metadata parser.
const MAX_METADATA_BODY_BYTES: u64 = 16 * 1024 * 1024;

/// Headers that carry a credential, a signature or the original address, and
/// are therefore never replayed onto a different origin.
const SENSITIVE_HEADERS: [&str; 5] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "destination",
    "x-amz-",
];

/// True when `name` names a header that must not cross an origin boundary.
fn is_sensitive(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    SENSITIVE_HEADERS
        .iter()
        .any(|marker| lowered.starts_with(marker))
}

/// An address split into the parts a request needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// True when the connection carries transport security.
    pub secure: bool,
    /// Host name or address.
    pub host: String,
    /// Port.
    pub port: u16,
    /// Path and query, starting with a separator.
    pub path: String,
}

impl Url {
    /// Split an absolute address.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] for a scheme that is not HTTP, and
    /// [`VfsError::Protocol`] for an address with no host.
    pub fn parse(raw: &str) -> VfsResult<Self> {
        let raw = raw.trim();
        let (scheme, rest) = raw
            .split_once("://")
            .ok_or_else(|| VfsError::protocol(format!("{raw:?} names no scheme")))?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            other => {
                return Err(VfsError::unsupported(format!(
                    "the scheme {other:?} is not an HTTP address"
                )))
            }
        };
        let (authority, path) = match rest.find('/') {
            Some(cut) => (
                rest.get(..cut).unwrap_or_default(),
                rest.get(cut..).unwrap_or("/"),
            ),
            None => (rest, "/"),
        };
        // Credentials in an address are refused rather than used, because a
        // password belongs in the secret store and never in a stored string.
        if authority.contains('@') {
            return Err(VfsError::protocol(
                "an address may not carry a user name or a password".to_owned(),
            ));
        }
        let (host, port) = split_authority(authority, secure)?;
        // A host reaches the request line and the host header, so a control
        // character in it would split the request.
        if host.is_empty()
            || host
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\0')
        {
            return Err(VfsError::protocol(
                "an address names no usable host".to_owned(),
            ));
        }
        Ok(Self {
            secure,
            host,
            port,
            path: if path.is_empty() {
                "/".to_owned()
            } else {
                path.to_owned()
            },
        })
    }

    /// The address with `path` in place of this one's path.
    #[must_use]
    pub fn with_path(&self, path: &str) -> Self {
        Self {
            path: if path.starts_with('/') {
                path.to_owned()
            } else {
                format!("/{path}")
            },
            ..self.clone()
        }
    }

    /// The scheme, host and port, with no path.
    #[must_use]
    pub fn origin(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        let standard = if self.secure { 443 } else { 80 };
        if self.port == standard {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }

    /// The whole address.
    #[must_use]
    pub fn to_text(&self) -> String {
        format!("{}{}", self.origin(), self.path)
    }

    /// The value of the host header.
    #[must_use]
    pub fn host_header(&self) -> String {
        let standard = if self.secure { 443 } else { 80 };
        if self.port == standard {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Resolve a redirect target against this address.
    ///
    /// # Errors
    /// Returns [`VfsError::Protocol`] when the target cannot be resolved and
    /// [`VfsError::Tls`] when a secure request would become a plain one.
    pub fn redirect(&self, location: &str) -> VfsResult<Self> {
        let target = if location.contains("://") {
            Self::parse(location)?
        } else if let Some(rest) = location.strip_prefix("//") {
            Self::parse(&format!(
                "{}://{rest}",
                if self.secure { "https" } else { "http" }
            ))?
        } else if location.starts_with('/') {
            self.with_path(location)
        } else {
            let base = self.path.rsplit_once('/').map_or("", |(head, _)| head);
            self.with_path(&format!("{base}/{location}"))
        };
        if self.secure && !target.secure {
            return Err(VfsError::tls(
                "the server redirected a secure request onto a plain connection".to_owned(),
            ));
        }
        Ok(target)
    }
}

/// Split a host and a port.
fn split_authority(authority: &str, secure: bool) -> VfsResult<(String, u16)> {
    let standard = if secure { 443 } else { 80 };
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| VfsError::protocol("an address has an unterminated host".to_owned()))?;
        let port = match tail.strip_prefix(':') {
            Some(digits) => digits
                .parse()
                .map_err(|_| VfsError::protocol(format!("{digits:?} is not a port")))?,
            None => standard,
        };
        return Ok((host.to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, digits)) if !host.is_empty() => {
            let port = digits
                .parse()
                .map_err(|_| VfsError::protocol(format!("{digits:?} is not a port")))?;
            Ok((host.to_owned(), port))
        }
        _ if authority.is_empty() => Err(VfsError::protocol("an address names no host".to_owned())),
        _ => Ok((authority.to_owned(), standard)),
    }
}

/// What a request carries as its body.
pub enum RequestBody<'a> {
    /// No body.
    Empty,
    /// A body already in memory.
    Bytes(Vec<u8>),
    /// A body of known length, streamed from a reader.
    Reader {
        /// Where the bytes come from.
        source: &'a mut dyn Read,
        /// How many bytes the reader yields.
        length: u64,
    },
}

impl std::fmt::Debug for RequestBody<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("RequestBody::Empty"),
            Self::Bytes(bytes) => write!(f, "RequestBody::Bytes({})", bytes.len()),
            Self::Reader { length, .. } => write!(f, "RequestBody::Reader({length})"),
        }
    }
}

impl RequestBody<'_> {
    /// How many bytes the body holds.
    const fn length(&self) -> u64 {
        match self {
            Self::Empty => 0,
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Reader { length, .. } => *length,
        }
    }

    /// True when the request needs no body at all.
    const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// The body of a response.
pub struct ResponseBody {
    transport: Transport,
    remaining: Option<u64>,
    chunked: bool,
    chunk_left: u64,
    done: bool,
}

impl std::fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseBody")
            .field("remaining", &self.remaining)
            .field("chunked", &self.chunked)
            .finish_non_exhaustive()
    }
}

impl Read for ResponseBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.chunked {
            if self.chunk_left == 0 {
                let line = read_line(&mut self.transport, MAX_CHUNK_LINE).map_err(to_io)?;
                let digits = line.split(';').next().unwrap_or_default().trim();
                // A sign or a prefix would make the radix parser accept a
                // value no chunk header may carry.
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(std::io::Error::other(
                        "the server sent a chunk length that is not a number",
                    ));
                }
                let size = u64::from_str_radix(digits, 16).map_err(|_| {
                    std::io::Error::other("the server sent a chunk length that is not a number")
                })?;
                if size > MAX_CHUNK_BYTES {
                    return Err(std::io::Error::other(
                        "the server sent a chunk length no transfer can mean",
                    ));
                }
                if size == 0 {
                    self.done = true;
                    return Ok(0);
                }
                self.chunk_left = size;
            }
            let want = buf
                .len()
                .min(usize::try_from(self.chunk_left).unwrap_or(buf.len()));
            let Some(slice) = buf.get_mut(..want) else {
                return Ok(0);
            };
            let read = self.transport.read(slice)?;
            if read == 0 {
                self.done = true;
                return Ok(0);
            }
            self.chunk_left -= read as u64;
            if self.chunk_left == 0 {
                // The line ending after a chunk carries nothing. Anything else
                // means the framing and the content disagree.
                let trailer = read_line(&mut self.transport, MAX_CHUNK_LINE).map_err(to_io)?;
                if !trailer.is_empty() {
                    return Err(std::io::Error::other(
                        "the server did not end a chunk with a line ending",
                    ));
                }
            }
            return Ok(read);
        }
        if let Some(left) = self.remaining {
            if left == 0 {
                self.done = true;
                return Ok(0);
            }
            let want = buf.len().min(usize::try_from(left).unwrap_or(buf.len()));
            let Some(slice) = buf.get_mut(..want) else {
                return Ok(0);
            };
            let read = self.transport.read(slice)?;
            if read == 0 {
                self.done = true;
                return Err(std::io::Error::other(
                    "the server closed the connection before the declared length",
                ));
            }
            self.remaining = Some(left.saturating_sub(read as u64));
            return Ok(read);
        }
        let read = self.transport.read(buf)?;
        if read == 0 {
            self.done = true;
        }
        Ok(read)
    }
}

/// Turn a crate error back into one a reader can report.
fn to_io(error: VfsError) -> std::io::Error {
    crate::limits::carry(error)
}

/// One response.
pub struct Response {
    /// Three digit status.
    pub status: u16,
    /// Header names, lowercased, and their values.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: ResponseBody,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl Response {
    /// The first value of `name`.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// True for a status that names success.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// Read a protocol metadata body under the smaller metadata ceiling.
    ///
    /// # Errors
    /// Returns [`VfsError::LimitExceeded`] when the body passes a ceiling.
    pub fn read_body(
        &mut self,
        limits: &crate::limits::Limits,
        cancel: &Cancel,
    ) -> VfsResult<Vec<u8>> {
        let bounded = metadata_body_limits(*limits);
        let budget = crate::limits::Budget::new(bounded.max_archive_bytes);
        crate::limits::read_bounded(&mut self.body, 0, &bounded, &budget, cancel)
    }
}

fn metadata_body_limits(mut limits: crate::limits::Limits) -> crate::limits::Limits {
    limits.max_entry_bytes = limits.max_entry_bytes.min(MAX_METADATA_BODY_BYTES);
    limits.max_archive_bytes = limits.max_archive_bytes.min(MAX_METADATA_BODY_BYTES);
    limits
}

/// Credentials for one server.
pub struct Credentials {
    /// Account name.
    pub username: String,
    /// Account password.
    pub password: Secret,
    /// Which scheme to offer.
    pub scheme: HttpAuthScheme,
    /// Allow the account name and password to travel over a plain connection.
    ///
    /// Basic authentication carries the password in a reversible encoding, so
    /// over plain HTTP it is readable by anything on the path. The profile
    /// states this; nothing sets it implicitly.
    pub allow_plaintext: bool,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &self.password)
            .field("scheme", &self.scheme)
            .field("allow_plaintext", &self.allow_plaintext)
            .finish()
    }
}

/// A client for one server.
pub struct HttpClient {
    tls: Arc<ClientConfig>,
    credentials: Option<Credentials>,
    timeout: std::time::Duration,
    preference: AddressPreference,
    digest: std::sync::Mutex<Option<DigestState>>,
}

/// What a digest answer carries forward from the previous request.
///
/// A server that checks the request count refuses a second answer that repeats
/// the count against one server nonce, so the count rises for as long as the
/// nonce stays the same and restarts when the server issues a new one.
#[derive(Debug)]
struct DigestState {
    /// The server nonce the count belongs to.
    nonce: String,
    /// The client nonce sent with every answer against that server nonce.
    cnonce: String,
    /// Number of answers sent against that server nonce so far.
    count: u32,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("credentials", &self.credentials)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// A client that verifies certificates as `options` says.
    ///
    /// # Errors
    /// Returns [`VfsError::Tls`] when the configuration is refused.
    pub fn new(
        options: &crate::remote::tls::TlsOptions,
        credentials: Option<Credentials>,
        timeout: std::time::Duration,
    ) -> VfsResult<Self> {
        if credentials
            .as_ref()
            .is_some_and(|credentials| matches!(&credentials.scheme, HttpAuthScheme::Unknown(_)))
        {
            return Err(VfsError::unsupported(
                "the profile uses an unsupported HTTP authentication scheme",
            ));
        }
        Ok(Self {
            tls: crate::remote::tls::client_config(options)?,
            credentials,
            timeout,
            preference: AddressPreference::Resolver,
            digest: std::sync::Mutex::new(None),
        })
    }

    /// Send one request, following redirects and answering one challenge.
    ///
    /// # Errors
    /// Returns [`VfsError::Network`] when the server cannot be reached,
    /// [`VfsError::Tls`] when a secure request would be redirected onto a
    /// plain one, and [`VfsError::Protocol`] for a reply that does not parse.
    pub fn send(
        &self,
        method: &str,
        url: &Url,
        headers: &[(String, String)],
        body: RequestBody<'_>,
        cancel: &Cancel,
    ) -> VfsResult<Response> {
        let mut target = url.clone();
        let mut body = body;
        let mut challenge: Option<String> = None;
        let origin = url.origin();
        // A credential and a signature are made for one origin. Once a server
        // moves the request to another one, neither is sent again.
        let mut carried: Vec<(String, String)> = headers.to_vec();
        let mut same_origin = true;
        for _ in 0..=MAX_REDIRECTS {
            let reusable_body = match &body {
                RequestBody::Empty => Some(RequestBody::Empty),
                RequestBody::Bytes(bytes) => Some(RequestBody::Bytes(bytes.clone())),
                RequestBody::Reader { .. } => None,
            };
            let response = self.once(
                method,
                &target,
                &carried,
                body,
                challenge.as_deref().filter(|_| same_origin),
                cancel,
                same_origin,
            )?;

            if response.status == 401 && challenge.is_none() && self.credentials.is_some() {
                if let Some(header) = response.header("www-authenticate") {
                    let header = header.to_owned();
                    let Some(next) = reusable_body else {
                        return Ok(response);
                    };
                    challenge = Some(header);
                    body = next;
                    continue;
                }
            }
            if !is_redirect(response.status) {
                return Ok(response);
            }
            let Some(location) = response.header("location").map(str::to_owned) else {
                return Ok(response);
            };
            let Some(next) = reusable_body else {
                return Ok(response);
            };
            target = target.redirect(&location)?;
            if target.origin() != origin {
                // Content written for one server is not handed to another one
                // that server names.
                if !next.is_empty() {
                    return Err(VfsError::protocol(format!(
                        "the server redirected a request that carries content to another \
                         server ({})",
                        target.origin()
                    )));
                }
                same_origin = false;
                carried.retain(|(name, _)| !is_sensitive(name));
            }
            // A challenge is answered against one address only.
            challenge = None;
            body = next;
        }
        Err(VfsError::protocol(format!(
            "the server redirected more than {MAX_REDIRECTS} times"
        )))
    }

    /// Send one request with no redirect handling.
    #[allow(
        clippy::too_many_arguments,
        reason = "one request, written out rather than hidden in a builder"
    )]
    fn once(
        &self,
        method: &str,
        url: &Url,
        headers: &[(String, String)],
        body: RequestBody<'_>,
        challenge: Option<&str>,
        cancel: &Cancel,
        same_origin: bool,
    ) -> VfsResult<Response> {
        let splits = |value: &str| {
            value
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\0')
        };
        if splits(method) || splits(&url.path) {
            return Err(VfsError::protocol(
                "a request line may not carry whitespace or a control character".to_owned(),
            ));
        }
        let deadline = Deadline::after(self.timeout);
        let socket = connect(&url.host, url.port, deadline, self.preference, cancel)?;
        let mut transport = Transport::Plain(PollStream::new(
            socket,
            deadline,
            cancel.clone(),
            "http request",
        )?);
        if url.secure {
            transport = transport.secure(&self.tls, &url.host)?;
        }

        let mut request = format!("{method} {} HTTP/1.1\r\n", url.path);
        push_header(&mut request, "Host", &url.host_header());
        push_header(&mut request, "Connection", "close");
        push_header(&mut request, "Accept-Encoding", "identity");
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        {
            push_header(&mut request, "User-Agent", "compare-all");
        }
        for (name, value) in headers {
            // The client writes its own framing headers; a caller copy would
            // give the request two of them.
            if name.eq_ignore_ascii_case("content-length")
                || name.eq_ignore_ascii_case("transfer-encoding")
                || name.eq_ignore_ascii_case("host")
                || name.eq_ignore_ascii_case("connection")
            {
                continue;
            }
            push_header(&mut request, name, value);
        }
        if same_origin {
            if let Some(value) = self.authorization(method, url, challenge)? {
                push_header(&mut request, "Authorization", &value);
            }
        }
        if !body.is_empty() {
            push_header(&mut request, "Content-Length", &body.length().to_string());
        }
        request.push_str("\r\n");
        transport
            .write_all(request.as_bytes())
            .map_err(crate::limits::uncarry)?;

        match body {
            RequestBody::Empty => {}
            RequestBody::Bytes(bytes) => transport
                .write_all(&bytes)
                .map_err(crate::limits::uncarry)?,
            RequestBody::Reader { source, length } => {
                let mut left = length;
                let mut buffer = vec![0u8; 64 * 1024];
                while left > 0 {
                    cancel.check()?;
                    let want = buffer
                        .len()
                        .min(usize::try_from(left).unwrap_or(buffer.len()));
                    let Some(slice) = buffer.get_mut(..want) else {
                        break;
                    };
                    let read = source.read(slice).map_err(crate::limits::uncarry)?;
                    if read == 0 {
                        return Err(VfsError::protocol(
                            "the body ended before the declared length".to_owned(),
                        ));
                    }
                    transport
                        .write_all(buffer.get(..read).unwrap_or_default())
                        .map_err(crate::limits::uncarry)?;
                    left -= read as u64;
                }
            }
        }
        transport.flush().map_err(crate::limits::uncarry)?;
        read_response(transport, method)
    }

    /// The authorization header for this request, where one applies.
    fn authorization(
        &self,
        method: &str,
        url: &Url,
        challenge: Option<&str>,
    ) -> VfsResult<Option<String>> {
        let Some(credentials) = self.credentials.as_ref() else {
            return Ok(None);
        };
        if credentials.scheme == HttpAuthScheme::None {
            return Ok(None);
        }
        if let Some(header) = challenge {
            if header
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("digest")
            {
                if credentials.scheme == HttpAuthScheme::Basic {
                    return Ok(None);
                }
                let Ok(mut state) = self.digest.lock() else {
                    return Err(VfsError::protocol(
                        "the digest state of this client is unusable".to_owned(),
                    ));
                };
                return Ok(digest_answer(
                    credentials,
                    method,
                    &url.path,
                    header,
                    &mut state,
                ));
            }
        }
        if credentials.scheme == HttpAuthScheme::Digest {
            // A digest profile only sends a digest response to a digest
            // challenge. It never falls back to sending the password.
            return Ok(None);
        }
        if !url.secure && !credentials.allow_plaintext {
            return Err(VfsError::tls(
                "the password would travel in the clear over a plain connection; use https or \
                 state that the profile accepts a plain connection"
                    .to_owned(),
            ));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            credentials.username,
            credentials.password.expose()
        ));
        Ok(Some(format!("Basic {encoded}")))
    }
}

/// True for a status that names another address.
const fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Append one header line, dropping anything that would split the request.
fn push_header(request: &mut String, name: &str, value: &str) {
    if name.contains(['\r', '\n', ':', '\0']) || value.contains(['\r', '\n', '\0']) {
        return;
    }
    request.push_str(name);
    request.push_str(": ");
    request.push_str(value);
    request.push_str("\r\n");
}

/// Read the status line, the headers and the framing of a response.
fn read_response(mut transport: Transport, method: &str) -> VfsResult<Response> {
    let status_line = read_line(&mut transport, MAX_HEADER_LINE)?;
    let mut parts = status_line.split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(VfsError::protocol(
            "the reply does not start with a status line".to_owned(),
        ));
    }
    let status: u16 = parts
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| VfsError::protocol("the reply carries no status".to_owned()))?;

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut ended = false;
    for _ in 0..MAX_HEADERS {
        let line = read_line(&mut transport, MAX_HEADER_LINE)?;
        if line.is_empty() {
            ended = true;
            break;
        }
        // A continuation line would let a value carry a name the split below
        // never sees, so it is refused rather than folded.
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(VfsError::protocol(
                "the reply folds a header across lines".to_owned(),
            ));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(VfsError::protocol(
                "the reply carries a header line with no name".to_owned(),
            ));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    if !ended {
        return Err(VfsError::protocol(format!(
            "the reply carries more than {MAX_HEADERS} headers"
        )));
    }

    // Two ways of framing one body let an intermediary and this client
    // disagree about where the body ends, so a reply that states both, or that
    // states one twice with different values, is refused.
    let encodings: Vec<&str> = headers
        .iter()
        .filter(|(name, _)| name == "transfer-encoding")
        .map(|(_, value)| value.as_str())
        .collect();
    let mut lengths = headers
        .iter()
        .filter(|(name, _)| name == "content-length")
        .map(|(_, value)| value.trim().parse::<u64>());
    let first_length = lengths.next().transpose().map_err(|_| {
        VfsError::protocol("the reply states a content length that is not a number".to_owned())
    })?;
    for other in lengths {
        if other.ok() != first_length {
            return Err(VfsError::protocol(
                "the reply states two different content lengths".to_owned(),
            ));
        }
    }
    let chunked = match encodings.len() {
        0 => false,
        1 => {
            let value = encodings.first().copied().unwrap_or_default();
            if !value.trim().eq_ignore_ascii_case("chunked") {
                return Err(VfsError::protocol(
                    "the reply names a transfer encoding this client does not decode".to_owned(),
                ));
            }
            true
        }
        _ => {
            return Err(VfsError::protocol(
                "the reply states a transfer encoding more than once".to_owned(),
            ))
        }
    };
    if chunked && first_length.is_some() {
        return Err(VfsError::protocol(
            "the reply frames its body with both a content length and a transfer encoding"
                .to_owned(),
        ));
    }
    let declared = first_length;
    let empty = method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status);
    let remaining = if empty { Some(0) } else { declared };

    Ok(Response {
        status,
        headers,
        body: ResponseBody {
            transport,
            remaining,
            chunked: chunked && !empty,
            chunk_left: 0,
            done: empty,
        },
    })
}

/// Build the answer to a digest challenge.
///
/// Only the quality of protection the common servers offer is implemented.
/// A challenge this does not understand produces no header, and the caller
/// sees the server's own refusal rather than a wrong answer.
fn digest_answer(
    credentials: &Credentials,
    method: &str,
    path: &str,
    challenge: &str,
    state: &mut Option<DigestState>,
) -> Option<String> {
    use md5::{Digest, Md5};
    use std::fmt::Write;

    let fields = parse_challenge(challenge);
    let realm = fields.iter().find(|(key, _)| key == "realm")?.1.clone();
    let nonce = fields.iter().find(|(key, _)| key == "nonce")?.1.clone();
    let opaque = fields
        .iter()
        .find(|(key, _)| key == "opaque")
        .map(|(_, value)| value.clone());
    let algorithm = fields
        .iter()
        .find(|(key, _)| key == "algorithm")
        .map_or("MD5", |(_, value)| value.as_str());
    if !algorithm.eq_ignore_ascii_case("MD5") {
        return None;
    }
    let qop = fields
        .iter()
        .find(|(key, _)| key == "qop")
        .map(|(_, value)| value.clone());

    let hash = |input: &str| -> String { crate::remote::tls::hex(&Md5::digest(input.as_bytes())) };
    let ha1 = hash(&format!(
        "{}:{realm}:{}",
        credentials.username,
        credentials.password.expose()
    ));
    let ha2 = hash(&format!("{method}:{path}"));

    let mut header = format!(
        "Digest username=\"{}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{path}\"",
        credentials.username
    );
    let response = if qop.as_deref().is_some_and(|value| value.contains("auth")) {
        let carried = match state {
            Some(previous) if previous.nonce == nonce => {
                previous.count = previous.count.saturating_add(1);
                previous
            }
            _ => state.insert(DigestState {
                nonce: nonce.clone(),
                cnonce: format!("{:016x}", crate::remote::nonce()),
                count: 1,
            }),
        };
        let cnonce = carried.cnonce.clone();
        let count = format!("{:08x}", carried.count);
        let value = hash(&format!("{ha1}:{nonce}:{count}:{cnonce}:auth:{ha2}"));
        let _ = write!(header, ", qop=auth, nc={count}, cnonce=\"{cnonce}\"");
        value
    } else {
        hash(&format!("{ha1}:{nonce}:{ha2}"))
    };
    let _ = write!(header, ", response=\"{response}\"");
    if let Some(value) = opaque {
        let _ = write!(header, ", opaque=\"{value}\"");
    }
    header.push_str(", algorithm=MD5");
    Some(header)
}

/// Split the comma separated fields of an authentication challenge.
fn parse_challenge(challenge: &str) -> Vec<(String, String)> {
    let body = challenge
        .trim()
        .strip_prefix("Digest")
        .or_else(|| challenge.trim().strip_prefix("digest"))
        .unwrap_or(challenge)
        .trim();
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let Some((key, tail)) = rest.split_once('=') else {
            break;
        };
        let key = key
            .trim()
            .trim_start_matches(',')
            .trim()
            .to_ascii_lowercase();
        let tail = tail.trim_start();
        let (value, next) = if let Some(quoted) = tail.strip_prefix('"') {
            match quoted.split_once('"') {
                Some((value, next)) => (value.to_owned(), next),
                None => (quoted.to_owned(), ""),
            }
        } else {
            match tail.split_once(',') {
                Some((value, next)) => (value.trim().to_owned(), next),
                None => (tail.trim().to_owned(), ""),
            }
        };
        out.push((key, value));
        rest = next.trim_start().trim_start_matches(',');
    }
    out
}

/// Percent-encode one path component.
#[must_use]
pub fn encode_component(value: &str) -> String {
    /// Characters kept as they are in a path component.
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(value, KEEP).to_string()
}

/// Decode a percent-encoded path component.
#[must_use]
pub fn decode_component(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn authenticated_client(scheme: HttpAuthScheme, allow_plaintext: bool) -> HttpClient {
        HttpClient::new(
            &crate::remote::tls::TlsOptions::default(),
            Some(Credentials {
                username: "user".to_owned(),
                password: Secret::new("hunter2"),
                scheme,
                allow_plaintext,
            }),
            std::time::Duration::from_secs(1),
        )
        .unwrap()
    }

    #[test]
    fn an_address_splits_into_its_parts() {
        let url = Url::parse("https://example.test/share/sub").unwrap();
        assert!(url.secure);
        assert_eq!(url.port, 443);
        assert_eq!(url.path, "/share/sub");
        assert_eq!(url.host_header(), "example.test");
        let plain = Url::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(plain.port, 8080);
        assert_eq!(plain.path, "/");
        assert_eq!(plain.host_header(), "127.0.0.1:8080");
    }

    #[test]
    fn an_address_with_credentials_or_a_foreign_scheme_is_refused() {
        assert!(Url::parse("https://user:pass@example.test/").is_err());
        assert!(Url::parse("ftp://example.test/").is_err());
        assert!(Url::parse("example.test/").is_err());
    }

    #[test]
    fn an_unknown_authentication_scheme_is_refused_before_a_request() {
        let credentials = Credentials {
            username: "operator".to_owned(),
            password: crate::remote::Secret::new("secret".to_owned()),
            scheme: HttpAuthScheme::Unknown("secret_handshake".to_owned()),
            allow_plaintext: false,
        };

        assert!(matches!(
            HttpClient::new(
                &crate::remote::tls::TlsOptions::default(),
                Some(credentials),
                std::time::Duration::from_secs(1),
            ),
            Err(VfsError::Unsupported { .. })
        ));
    }

    #[test]
    fn a_secure_request_is_never_redirected_onto_a_plain_one() {
        let url = Url::parse("https://example.test/a/b").unwrap();
        assert_eq!(url.redirect("/c").unwrap().path, "/c");
        assert_eq!(url.redirect("c").unwrap().path, "/a/c");
        assert!(matches!(
            url.redirect("http://example.test/c"),
            Err(VfsError::Tls { .. })
        ));
        let plain = Url::parse("http://example.test/").unwrap();
        assert!(plain.redirect("https://example.test/").is_ok());
    }

    #[test]
    fn a_digest_challenge_parses() {
        let fields =
            parse_challenge("Digest realm=\"test\", qop=\"auth\", nonce=\"abc\", opaque=\"xyz\"");
        assert_eq!(fields.len(), 4);
        assert_eq!(fields.first().unwrap().1, "test");
    }

    #[test]
    fn a_digest_answer_carries_no_password() {
        let credentials = Credentials {
            username: "user".to_owned(),
            password: Secret::new("hunter2"),
            scheme: HttpAuthScheme::Digest,
            allow_plaintext: false,
        };
        let header = digest_answer(
            &credentials,
            "GET",
            "/file",
            "Digest realm=\"test\", qop=\"auth\", nonce=\"abc\"",
            &mut None,
        )
        .unwrap();
        assert!(header.contains("username=\"user\""));
        assert!(!header.contains("hunter2"));
        assert!(format!("{credentials:?}").contains("***"));
        assert!(!format!("{credentials:?}").contains("hunter2"));
    }

    #[test]
    fn a_basic_profile_does_not_answer_a_digest_challenge() {
        let client = authenticated_client(HttpAuthScheme::Basic, false);
        let url = Url::parse("http://example.test/file").unwrap();

        assert!(client
            .authorization(
                "GET",
                &url,
                Some("Digest realm=\"test\", qop=\"auth\", nonce=\"abc\""),
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_basic_profile_still_uses_basic_over_tls() {
        let client = authenticated_client(HttpAuthScheme::Basic, false);
        let url = Url::parse("https://example.test/file").unwrap();

        assert!(client
            .authorization("GET", &url, None)
            .unwrap()
            .is_some_and(|header| header.starts_with("Basic ")));
    }

    #[test]
    fn an_explicit_digest_profile_still_answers_a_digest_challenge() {
        let client = authenticated_client(HttpAuthScheme::Digest, false);
        let url = Url::parse("http://example.test/file").unwrap();

        assert!(client
            .authorization(
                "GET",
                &url,
                Some("Digest realm=\"test\", qop=\"auth\", nonce=\"abc\""),
            )
            .unwrap()
            .is_some_and(|header| header.starts_with("Digest ")));
    }

    #[test]
    fn a_digest_profile_does_not_answer_a_basic_challenge() {
        let client = authenticated_client(HttpAuthScheme::Digest, false);
        let url = Url::parse("https://example.test/file").unwrap();

        assert!(client
            .authorization("GET", &url, Some("Basic realm=\"test\""))
            .unwrap()
            .is_none());
    }

    #[test]
    fn negotiate_profiles_still_answer_a_digest_challenge() {
        let client = authenticated_client(HttpAuthScheme::Negotiate, false);
        let url = Url::parse("https://example.test/file").unwrap();

        assert!(client
            .authorization(
                "GET",
                &url,
                Some("Digest realm=\"test\", qop=\"auth\", nonce=\"abc\""),
            )
            .unwrap()
            .is_some_and(|header| header.starts_with("Digest ")));
    }

    #[test]
    fn a_basic_profile_refuses_plain_http_before_authenticating() {
        let client = authenticated_client(HttpAuthScheme::Basic, false);
        let url = Url::parse("http://example.test/file").unwrap();

        assert!(matches!(
            client.authorization("GET", &url, None),
            Err(VfsError::Tls { .. })
        ));
    }

    fn digest_credentials() -> Credentials {
        Credentials {
            username: "user".to_owned(),
            password: Secret::new("hunter2"),
            scheme: HttpAuthScheme::Digest,
            allow_plaintext: false,
        }
    }

    /// The `cnonce` value of one answer.
    fn client_nonce(header: &str) -> String {
        header
            .split("cnonce=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn the_request_count_rises_across_answers_to_one_server_nonce() {
        let credentials = digest_credentials();
        let challenge = "Digest realm=\"test\", qop=\"auth\", nonce=\"abc\"";
        let mut state = None;
        let first = digest_answer(&credentials, "GET", "/one", challenge, &mut state).unwrap();
        let second = digest_answer(&credentials, "GET", "/two", challenge, &mut state).unwrap();
        let third = digest_answer(&credentials, "GET", "/three", challenge, &mut state).unwrap();
        assert!(first.contains("nc=00000001"), "{first}");
        assert!(second.contains("nc=00000002"), "{second}");
        assert!(third.contains("nc=00000003"), "{third}");
        assert_eq!(client_nonce(&first), client_nonce(&second));
    }

    #[test]
    fn a_new_server_nonce_restarts_the_request_count() {
        let credentials = digest_credentials();
        let mut state = None;
        let first = digest_answer(
            &credentials,
            "GET",
            "/one",
            "Digest realm=\"test\", qop=\"auth\", nonce=\"abc\"",
            &mut state,
        )
        .unwrap();
        let second = digest_answer(
            &credentials,
            "GET",
            "/two",
            "Digest realm=\"test\", qop=\"auth\", nonce=\"def\"",
            &mut state,
        )
        .unwrap();
        assert!(first.contains("nc=00000001"), "{first}");
        assert!(second.contains("nc=00000001"), "{second}");
        assert_ne!(client_nonce(&first), client_nonce(&second));
    }

    #[test]
    fn a_path_component_round_trips() {
        assert_eq!(encode_component("a b/c"), "a%20b%2Fc");
        assert_eq!(decode_component("a%20b%2Fc"), "a b/c");
    }

    #[test]
    fn a_metadata_response_cannot_use_the_file_transfer_ceiling() {
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let bytes = vec![
                0_u8;
                usize::try_from(MAX_METADATA_BODY_BYTES)
                    .unwrap_or_default()
                    .saturating_add(1)
            ];
            // The reader stops at the metadata ceiling and closes its side.
            // A broken pipe at that point is expected.
            let _ = stream.write_all(&bytes);
        });

        let stream = TcpStream::connect(address).unwrap();
        let transport = Transport::Plain(
            PollStream::new(stream, Deadline::never(), Cancel::new(), "test response").unwrap(),
        );
        let mut response = Response {
            status: 200,
            headers: Vec::new(),
            body: ResponseBody {
                transport,
                remaining: None,
                chunked: false,
                chunk_left: 0,
                done: false,
            },
        };

        let error = response
            .read_body(&crate::limits::Limits::default(), &Cancel::new())
            .unwrap_err();
        sender.join().unwrap();
        assert!(matches!(
            error,
            VfsError::LimitExceeded {
                kind: crate::error::LimitKind::EntrySize,
                limit: MAX_METADATA_BODY_BYTES
            }
        ));
    }
}
