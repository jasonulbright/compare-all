//! A small HTTP/1.1 server, and the two handlers the remote tests need.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{Stop, TestCertificate};

/// One request the handler sees.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Request verb.
    pub method: String,
    /// Path, still percent-encoded.
    pub path: String,
    /// Query, without the question mark.
    pub query: String,
    /// Header names, lowercased, and their values.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Request {
    /// The first value of `name`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The value of one query parameter.
    pub fn parameter(&self, name: &str) -> Option<String> {
        for pair in self.query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if decode(key) == name {
                return Some(decode(value));
            }
        }
        None
    }

    /// The path, decoded.
    pub fn decoded_path(&self) -> String {
        decode(&self.path)
    }
}

/// One reply the handler returns.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Three digit status.
    pub status: u16,
    /// Header names and their values.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Reply {
    /// A reply with a status and nothing else.
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A reply carrying a body.
    pub fn body(status: u16, kind: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_owned(), kind.to_owned())],
            body,
        }
    }

    /// The same reply with one more header.
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// A running server.
pub struct HttpTestServer {
    port: u16,
    stop: Stop,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The certificate the server offers, when it speaks transport security.
    pub certificate: Option<Arc<TestCertificate>>,
}

impl HttpTestServer {
    /// Start a server that answers with `handler`.
    pub fn start<H>(handler: H, certificate: Option<Arc<TestCertificate>>) -> Self
    where
        H: Fn(&Request) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Stop::new();
        let handler = Arc::new(handler);

        let thread = {
            let stop = stop.clone();
            let certificate = certificate.clone();
            std::thread::spawn(move || {
                while !stop.raised() {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let handler = Arc::clone(&handler);
                            let certificate = certificate.clone();
                            std::thread::spawn(move || {
                                let _ = serve(socket, handler.as_ref(), certificate.as_deref());
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        Self {
            port,
            stop,
            thread: Some(thread),
            certificate,
        }
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The address the server answers at.
    pub fn url(&self) -> String {
        let scheme = if self.certificate.is_some() {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://localhost:{}", self.port)
    }
}

impl Drop for HttpTestServer {
    fn drop(&mut self) {
        self.stop.raise();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A connection, with or without transport security.
enum Channel {
    Plain(TcpStream),
    Secure(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Read for Channel {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Secure(stream) => stream.read(buf),
        }
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Secure(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Secure(stream) => stream.flush(),
        }
    }
}

/// Serve one request.
fn serve(
    socket: TcpStream,
    handler: &(dyn Fn(&Request) -> Reply + Send + Sync),
    certificate: Option<&TestCertificate>,
) -> std::io::Result<()> {
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut channel = match certificate {
        Some(certificate) => {
            let connection = rustls::ServerConnection::new(certificate.server_config())
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            Channel::Secure(Box::new(rustls::StreamOwned::new(connection, socket)))
        }
        None => Channel::Plain(socket),
    };

    let Some(request) = read_request(&mut channel)? else {
        return Ok(());
    };
    let reply = handler(&request);
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    head.push_str("Connection: close\r\n\r\n");
    channel.write_all(head.as_bytes())?;
    if request.method != "HEAD" {
        channel.write_all(&reply.body)?;
    }
    channel.flush()
}

/// Read one request.
fn read_request(channel: &mut Channel) -> std::io::Result<Option<Request>> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    while !buffer.ends_with(b"\r\n\r\n") {
        match channel.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => buffer.push(byte[0]),
            Err(_) => return Ok(None),
        }
        if buffer.len() > 64 * 1024 {
            return Ok(None);
        }
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        channel.read_exact(&mut body)?;
    }
    Ok(Some(Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        body,
    }))
}

/// The reason phrase for a status.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        301 => "Moved Permanently",
        302 => "Found",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        _ => "Status",
    }
}

/// Decode a percent-encoded value.
pub fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let digits = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(digits, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve a request path under `root`, refusing anything that leaves it.
pub fn under(root: &Path, path: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for part in decode(path).split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// The range a request asks for, as a start offset.
pub fn range_start(request: &Request) -> Option<u64> {
    let value = request.header("range")?;
    let rest = value.trim().strip_prefix("bytes=")?;
    let (start, _) = rest.split_once('-')?;
    start.parse().ok()
}
