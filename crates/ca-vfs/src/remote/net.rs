//! Socket plumbing shared by the remote file systems.
//!
//! Every network call is bounded twice: by a deadline, so a server that
//! accepts a connection and then says nothing cannot hold a worker, and by the
//! caller's [`Cancel`], so a scan that the user stops releases its sockets
//! without waiting for the deadline.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::cancel::Cancel;
use crate::error::{VfsError, VfsResult};
use crate::limits::carry;

/// How long a blocking socket call waits before the cancellation flag and the
/// deadline are examined again.
const POLL_SLICE: Duration = Duration::from_millis(200);

/// A point in time a call must finish by.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    at: Option<Instant>,
    span: Option<Duration>,
}

impl Deadline {
    /// A deadline `span` from now.
    #[must_use]
    pub fn after(span: Duration) -> Self {
        Self {
            at: Instant::now().checked_add(span),
            span: Some(span),
        }
    }

    /// A deadline that never passes.
    #[must_use]
    pub const fn never() -> Self {
        Self {
            at: None,
            span: None,
        }
    }

    /// The same span again from now. A transfer renews its deadline on every
    /// byte it moves, so a long transfer that keeps moving is not cut while a
    /// stalled one still ends one span after its last byte.
    #[must_use]
    pub fn renewed(self) -> Self {
        self.span.map_or(self, Self::after)
    }

    /// Time left, or `None` when the deadline never passes.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.at
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// True once the deadline has passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.remaining().is_some_and(|left| left.is_zero())
    }

    /// Fail when the deadline has passed.
    ///
    /// # Errors
    /// Returns [`VfsError::Timeout`] naming `operation`.
    pub fn check(&self, operation: &str) -> VfsResult<()> {
        if self.passed() {
            return Err(VfsError::timeout(operation.to_owned()));
        }
        Ok(())
    }
}

/// How a host name resolves to addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressPreference {
    /// Try every address in the order the resolver returned them.
    #[default]
    Resolver,
    /// Try IPv6 addresses before IPv4 addresses.
    Ipv6First,
    /// Use IPv4 addresses only.
    Ipv4Only,
}

/// Resolve `host` and `port` and connect to the first address that answers.
///
/// # Errors
/// Returns [`VfsError::Network`] when nothing resolves or nothing answers, and
/// [`VfsError::Timeout`] when the deadline passes first.
pub fn connect(
    host: &str,
    port: u16,
    deadline: Deadline,
    preference: AddressPreference,
    cancel: &Cancel,
) -> VfsResult<TcpStream> {
    cancel.check()?;
    let mut addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| VfsError::network(format!("cannot resolve {host}: {error}")))?
        .collect();
    match preference {
        AddressPreference::Resolver => {}
        AddressPreference::Ipv6First => {
            addresses.sort_by_key(|address| u8::from(address.is_ipv4()));
        }
        AddressPreference::Ipv4Only => addresses.retain(SocketAddr::is_ipv4),
    }
    if addresses.is_empty() {
        return Err(VfsError::network(format!(
            "{host} resolves to no usable address"
        )));
    }

    let mut last = String::new();
    for address in addresses {
        cancel.check()?;
        deadline.check("connect")?;
        let slice = deadline.remaining().unwrap_or(POLL_SLICE).max(POLL_SLICE);
        match TcpStream::connect_timeout(&address, slice) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) => last = error.to_string(),
        }
    }
    Err(VfsError::network(format!("cannot reach {host}: {last}")))
}

/// A socket that yields to the cancellation flag and the deadline.
///
/// The socket keeps a short read timeout and the wrapper loops, so a transfer
/// stops within one poll slice of a cancel instead of waiting for the server.
#[derive(Debug)]
pub struct PollStream {
    inner: TcpStream,
    deadline: Deadline,
    cancel: Cancel,
    operation: &'static str,
}

impl PollStream {
    /// Wrap `inner`.
    ///
    /// # Errors
    /// Returns [`VfsError::Io`] when the socket refuses a timeout.
    pub fn new(
        inner: TcpStream,
        deadline: Deadline,
        cancel: Cancel,
        operation: &'static str,
    ) -> VfsResult<Self> {
        inner.set_read_timeout(Some(POLL_SLICE))?;
        inner.set_write_timeout(Some(POLL_SLICE))?;
        Ok(Self {
            inner,
            deadline,
            cancel,
            operation,
        })
    }

    /// Replace the deadline for the calls that follow.
    pub fn set_deadline(&mut self, deadline: Deadline) {
        self.deadline = deadline;
    }

    /// The deadline in force.
    #[must_use]
    pub fn deadline(&self) -> Deadline {
        self.deadline
    }

    /// The wrapped socket.
    #[must_use]
    pub fn socket(&self) -> &TcpStream {
        &self.inner
    }

    /// Whether the stalled call should be retried or reported.
    fn stalled(&self) -> Option<io::Error> {
        if self.cancel.is_cancelled() {
            return Some(carry(VfsError::Cancelled));
        }
        if self.deadline.passed() {
            return Some(carry(VfsError::timeout(self.operation.to_owned())));
        }
        None
    }
}

/// True for the errors a socket reports when its own timeout expired.
fn is_stall(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

impl Read for PollStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(error) = self.stalled() {
                return Err(error);
            }
            match self.inner.read(buf) {
                Ok(read) => {
                    if read > 0 {
                        self.deadline = self.deadline.renewed();
                    }
                    return Ok(read);
                }
                Err(error) if is_stall(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Write for PollStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            if let Some(error) = self.stalled() {
                return Err(error);
            }
            match self.inner.write(buf) {
                Ok(written) => {
                    if written > 0 {
                        self.deadline = self.deadline.renewed();
                    }
                    return Ok(written);
                }
                Err(error) if is_stall(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        loop {
            if let Some(error) = self.stalled() {
                return Err(error);
            }
            match self.inner.flush() {
                Ok(()) => return Ok(()),
                Err(error) if is_stall(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

/// A connection, with or without transport security.
#[cfg(feature = "tls")]
pub enum Transport {
    /// A plain socket.
    Plain(PollStream),
    /// A socket with transport security over it.
    Secure(Box<rustls::StreamOwned<rustls::ClientConnection, PollStream>>),
    /// No socket. A transport holds this only while it is being replaced by a
    /// secured one; every call on it reports a closed connection.
    Closed,
}

#[cfg(feature = "tls")]
impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain(_) => f.write_str("Transport::Plain"),
            Self::Secure(_) => f.write_str("Transport::Secure"),
            Self::Closed => f.write_str("Transport::Closed"),
        }
    }
}

#[cfg(feature = "tls")]
/// What a call on a closed transport reports.
fn closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        "the connection is not connected",
    )
}

#[cfg(feature = "tls")]
impl Transport {
    /// Replace the deadline the underlying socket enforces.
    pub fn set_deadline(&mut self, deadline: Deadline) {
        match self {
            Self::Plain(stream) => stream.set_deadline(deadline),
            Self::Secure(stream) => stream.get_mut().set_deadline(deadline),
            Self::Closed => {}
        }
    }

    /// The socket under the transport.
    #[must_use]
    pub fn socket(&self) -> Option<&TcpStream> {
        match self {
            Self::Plain(stream) => Some(stream.socket()),
            Self::Secure(stream) => Some(stream.get_ref().socket()),
            Self::Closed => None,
        }
    }

    /// Wrap the socket with transport security for `host`.
    ///
    /// # Errors
    /// Returns [`VfsError::Tls`] when the handshake cannot start.
    pub fn secure(
        self,
        config: &std::sync::Arc<rustls::ClientConfig>,
        host: &str,
    ) -> VfsResult<Self> {
        let stream = match self {
            Self::Plain(stream) => stream,
            other => return Ok(other),
        };
        let name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|_| VfsError::tls(format!("{host} is not a usable server name")))?;
        let connection = rustls::ClientConnection::new(std::sync::Arc::clone(config), name)
            .map_err(|error| VfsError::tls(error.to_string()))?;
        Ok(Self::Secure(Box::new(rustls::StreamOwned::new(
            connection, stream,
        ))))
    }

    /// Run the handshake to completion instead of leaving it to the first
    /// read or write.
    ///
    /// # Errors
    /// Propagates the transport failure as an I/O error.
    pub fn finish_handshake(&mut self) -> io::Result<()> {
        let Self::Secure(stream) = self else {
            return Ok(());
        };
        while stream.conn.is_handshaking() {
            let (read, written) = stream.conn.complete_io(&mut stream.sock)?;
            if read == 0 && written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the peer closed the connection during the handshake",
                ));
            }
        }
        Ok(())
    }

    /// Whether the completed handshake resumed a stored session.
    ///
    /// `None` on a plain transport and before the handshake completes.
    #[must_use]
    pub fn resumed(&self) -> Option<bool> {
        let Self::Secure(stream) = self else {
            return None;
        };
        stream
            .conn
            .handshake_kind()
            .map(|kind| kind == rustls::HandshakeKind::Resumed)
    }
}

#[cfg(feature = "tls")]
impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Secure(stream) => stream.read(buf),
            Self::Closed => Err(closed()),
        }
    }
}

#[cfg(feature = "tls")]
impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Secure(stream) => stream.write(buf),
            Self::Closed => Err(closed()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Secure(stream) => stream.flush(),
            Self::Closed => Err(closed()),
        }
    }
}

/// Read one line ending in `\n` from `source`, without the line ending.
///
/// The line is bounded so a server that never sends a line ending cannot grow
/// the buffer without limit.
///
/// # Errors
/// Returns [`VfsError::Protocol`] when the line passes `limit` bytes, and
/// [`VfsError::Network`] when the stream ends first.
pub fn read_line<R: Read>(source: &mut R, limit: usize) -> VfsResult<String> {
    let mut out: Vec<u8> = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        let read = source.read(&mut byte).map_err(crate::limits::uncarry)?;
        if read == 0 {
            if out.is_empty() {
                return Err(VfsError::network("the server closed the connection"));
            }
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        if out.len() >= limit {
            return Err(VfsError::protocol(format!(
                "a reply line passed {limit} bytes"
            )));
        }
        out.push(byte[0]);
    }
    while out.last() == Some(&b'\r') {
        out.pop();
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_passed_deadline_reports_the_operation() {
        let deadline = Deadline::after(Duration::from_millis(0));
        let error = deadline.check("listing").unwrap_err();
        assert!(matches!(error, VfsError::Timeout { .. }));
        assert!(error.to_string().contains("listing"));
        assert!(!Deadline::never().passed());
    }

    #[test]
    fn a_line_longer_than_the_limit_is_refused() {
        let data = vec![b'x'; 4096];
        let mut cursor = std::io::Cursor::new(data);
        assert!(matches!(
            read_line(&mut cursor, 64),
            Err(VfsError::Protocol { .. })
        ));
    }
}
