//! One logged-in control connection and the data channels it opens.

use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::ClientConfig;

use crate::cancel::Cancel;
use crate::error::{VfsError, VfsResult};
use crate::remote::net::{connect, read_line, AddressPreference, Deadline, PollStream};
use crate::remote::profile::{FtpProfile, FtpProtocol, ListingEncoding};
use crate::remote::secret::Secret;

/// Most bytes one reply line may hold.
const MAX_REPLY_LINE: usize = 8 * 1024;

/// Most lines one reply may hold before the server is treated as broken.
const MAX_REPLY_LINES: usize = 1024;

/// One reply from the server.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Three digit status.
    pub code: u16,
    /// Text of the reply, with the status digits removed from each line.
    pub text: String,
}

impl Reply {
    /// True when the status names a completed or an intermediate step.
    #[must_use]
    pub const fn is_positive(&self) -> bool {
        self.code < 400
    }
}

/// A control or data channel, with or without transport security.
pub use crate::remote::net::Transport as Channel;

/// What the server said it can do.
#[derive(Debug, Clone, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a record of independent server capabilities"
)]
pub struct Features {
    /// The machine-readable listing command.
    pub mlsd: bool,
    /// The modification time query.
    pub mdtm: bool,
    /// The modification time assignment.
    pub mfmt: bool,
    /// The size query.
    pub size: bool,
    /// Restarting a transfer at an offset.
    pub rest: bool,
    /// The extended passive command.
    pub epsv: bool,
    /// Naming the virtual host before the login.
    pub host: bool,
    /// UTF-8 names.
    pub utf8: bool,
}

impl Features {
    /// Read a feature list reply.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            let word = line.split_whitespace().next().unwrap_or_default();
            match word.to_ascii_uppercase().as_str() {
                "MLSD" | "MLST" => out.mlsd = true,
                "MDTM" => out.mdtm = true,
                "MFMT" => out.mfmt = true,
                "SIZE" => out.size = true,
                "REST" => out.rest = true,
                "EPSV" => out.epsv = true,
                "HOST" => out.host = true,
                "UTF8" => out.utf8 = true,
                _ => {}
            }
        }
        out
    }
}

/// One logged-in control connection.
pub struct Session {
    control: Channel,
    host: String,
    port: u16,
    tls: Option<Arc<ClientConfig>>,
    passive: bool,
    protect_data: bool,
    /// Open a data connection to the address the server names instead of to
    /// the peer of the control connection.
    trust_passive_address: bool,
    encoding: ListingEncoding,
    preference: AddressPreference,
    /// The last time a command was sent, for the keep-alive.
    used: Instant,
    /// What the server answered the feature query with.
    pub features: Features,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("passive", &self.passive)
            .field("features", &self.features)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Open a control connection and log in.
    ///
    /// # Errors
    /// Returns [`VfsError::Network`] when the server cannot be reached,
    /// [`VfsError::Tls`] when the channel cannot be secured, and
    /// [`VfsError::AuthFailed`] when the server refuses the account. No secret
    /// reaches any of those messages.
    pub fn open(
        settings: &FtpProfile,
        password: Option<&Secret>,
        tls: Option<Arc<ClientConfig>>,
        deadline: Deadline,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        let host = settings.login.host.clone();
        let port = settings
            .login
            .port
            .unwrap_or_else(|| settings.login.protocol.default_port());
        let preference = AddressPreference::from(settings.connection.address_family.clone());
        let socket = connect(&host, port, deadline, preference, cancel)?;
        let control = Channel::Plain(PollStream::new(
            socket,
            deadline,
            cancel.clone(),
            "control channel",
        )?);

        let mut session = Self {
            control,
            host,
            port,
            tls,
            passive: settings.connection.passive,
            protect_data: settings.login.protocol.uses_tls()
                && !settings.connection.clear_data_channel,
            trust_passive_address: settings.connection.trust_passive_address,
            encoding: settings.server.encoding.clone(),
            preference,
            used: Instant::now(),
            features: Features::default(),
        };

        if settings.login.protocol == FtpProtocol::FtpsImplicit {
            session.upgrade()?;
        }
        let greeting = session.read_reply()?;
        if !greeting.is_positive() {
            return Err(VfsError::network(format!(
                "the server refused the connection: {}",
                one_line(&greeting.text)
            )));
        }
        if settings.login.protocol == FtpProtocol::FtpsExplicit {
            let reply = session.command("AUTH TLS")?;
            if !reply.is_positive() {
                return Err(VfsError::tls(format!(
                    "the server refused to secure the control channel: {}",
                    one_line(&reply.text)
                )));
            }
            session.upgrade()?;
        }

        session.features = match session.command("FEAT") {
            Ok(reply) if reply.is_positive() => Features::parse(&reply.text),
            _ => Features::default(),
        };
        if session.features.utf8 {
            let _ = session.command("OPTS UTF8 ON");
        }
        session.login(settings, password)?;

        if settings.login.protocol.uses_tls() {
            if settings.connection.clear_data_channel {
                let _ = session.command("PROT C");
            } else {
                let _ = session.command("PBSZ 0");
                let reply = session.command("PROT P")?;
                // Carrying on without protection would move the content onto a
                // connection nothing defends, silently.
                if !reply.is_positive() {
                    return Err(VfsError::tls(format!(
                        "the server refused to secure the data channel: {}",
                        one_line(&reply.text)
                    )));
                }
                session.protect_data = true;
            }
            if settings.connection.clear_control_channel {
                let _ = session.command("CCC");
            }
        }
        let _ = session.command("TYPE I");
        for extra in &settings.server.custom_login_commands {
            if extra.trim().is_empty() {
                continue;
            }
            let _ = session.command(extra.trim());
        }
        Ok(session)
    }

    /// Wrap the control channel with transport security.
    fn upgrade(&mut self) -> VfsResult<()> {
        let Some(config) = self.tls.clone() else {
            return Err(VfsError::tls(
                "the profile asks for a secure channel and no configuration was built",
            ));
        };
        let taken = std::mem::replace(&mut self.control, Channel::Closed);
        self.control = taken.secure(&config, &self.host)?;
        Ok(())
    }

    /// Send the account name and password.
    fn login(&mut self, settings: &FtpProfile, password: Option<&Secret>) -> VfsResult<()> {
        if settings.connection.use_host_before_login && self.features.host {
            let host = self.host.clone();
            let _ = self.command(&format!("HOST {host}"));
        }
        let user = if settings.login.anonymous || settings.login.username.is_empty() {
            "anonymous".to_owned()
        } else {
            settings.login.username.clone()
        };
        let reply = self.command(&format!("USER {user}"))?;
        let reply = if reply.code == 331 || reply.code == 332 {
            let value = match password {
                Some(secret) => secret.expose().to_owned(),
                None if settings.login.anonymous || settings.login.username.is_empty() => {
                    settings.global.anonymous_login_email.clone()
                }
                None => String::new(),
            };
            // The command line is built here and dropped here; nothing formats
            // it into an error, a log line or a reply.
            self.send_raw(&format!("PASS {value}"))?;
            self.read_reply()?
        } else {
            reply
        };
        if !reply.is_positive() {
            return Err(VfsError::auth_failed(format!(
                "the server refused the account {user:?}: {}",
                one_line(&reply.text)
            )));
        }
        Ok(())
    }

    /// Send a command and read the reply.
    ///
    /// # Errors
    /// Returns [`VfsError::Network`] or [`VfsError::Protocol`] when the
    /// exchange fails.
    pub fn command(&mut self, line: &str) -> VfsResult<Reply> {
        self.send_raw(line)?;
        self.read_reply()
    }

    /// Send a command and fail when the reply is not positive.
    ///
    /// # Errors
    /// Returns [`VfsError::Protocol`] carrying the server's own text.
    pub fn expect(&mut self, line: &str) -> VfsResult<Reply> {
        let reply = self.command(line)?;
        if !reply.is_positive() {
            let verb = line.split_whitespace().next().unwrap_or("command");
            return Err(VfsError::protocol(format!(
                "{verb} failed: {}",
                one_line(&reply.text)
            )));
        }
        Ok(reply)
    }

    /// Write one command line.
    fn send_raw(&mut self, line: &str) -> VfsResult<()> {
        if line.contains('\r') || line.contains('\n') {
            return Err(VfsError::protocol(
                "a command may not carry a line ending".to_owned(),
            ));
        }
        self.used = Instant::now();
        self.control
            .write_all(format!("{line}\r\n").as_bytes())
            .map_err(crate::limits::uncarry)?;
        self.control.flush().map_err(crate::limits::uncarry)?;
        Ok(())
    }

    /// Read one reply, joining the lines of a multi-line reply.
    ///
    /// # Errors
    /// Returns [`VfsError::Protocol`] when the reply has no status digits or
    /// runs past the line ceiling.
    pub fn read_reply(&mut self) -> VfsResult<Reply> {
        let first = read_line(&mut self.control, MAX_REPLY_LINE)?;
        let code = status_of(&first).ok_or_else(|| {
            VfsError::protocol(format!("the reply {:?} carries no status", trim(&first)))
        })?;
        let mut text = String::from(first.get(4..).unwrap_or_default());
        if first.as_bytes().get(3) != Some(&b'-') {
            return Ok(Reply { code, text });
        }
        let terminator = format!("{code} ");
        for _ in 0..MAX_REPLY_LINES {
            let line = read_line(&mut self.control, MAX_REPLY_LINE)?;
            if line.starts_with(&terminator) {
                text.push('\n');
                text.push_str(line.get(4..).unwrap_or_default());
                return Ok(Reply { code, text });
            }
            text.push('\n');
            text.push_str(&line);
        }
        Err(VfsError::protocol(format!(
            "a reply ran past {MAX_REPLY_LINES} lines"
        )))
    }

    /// Send a keep-alive when the connection has been idle for `idle`.
    ///
    /// # Errors
    /// Returns whatever the exchange reports.
    pub fn keep_alive(&mut self, idle: Duration) -> VfsResult<()> {
        if idle.is_zero() || self.used.elapsed() < idle {
            return Ok(());
        }
        self.command("NOOP").map(|_| ())
    }

    /// Replace the deadline for the calls that follow.
    pub fn set_deadline(&mut self, deadline: Deadline) {
        self.control.set_deadline(deadline);
    }

    /// The encoding listings are read with.
    #[must_use]
    pub fn encoding(&self) -> ListingEncoding {
        self.encoding.clone()
    }

    /// Open a data channel and send `command` over the control channel.
    ///
    /// # Errors
    /// Returns [`VfsError::Network`] when the data channel cannot be made and
    /// [`VfsError::Protocol`] when the server refuses the command.
    pub fn open_data(
        &mut self,
        command: &str,
        deadline: Deadline,
        cancel: &Cancel,
        active_range: Option<(u16, u16)>,
    ) -> VfsResult<Channel> {
        if self.passive {
            let address = self.passive_address()?;
            let socket = connect(&address.0, address.1, deadline, self.preference, cancel)?;
            let reply = self.command(command)?;
            if !reply.is_positive() {
                return Err(reply_error(command, &reply));
            }
            let stream = PollStream::new(socket, deadline, cancel.clone(), "data channel")?;
            return self.protect(Channel::Plain(stream));
        }

        let announced = self.control_local_address()?;
        let listener = bind_active(active_range, &announced)?;
        let local = listener
            .local_addr()
            .map_err(|error| VfsError::network(error.to_string()))?;
        let expected = self.control_local_peer().ok();
        let port = local.port();
        let request = if announced.contains(':') {
            format!("EPRT |2|{announced}|{port}|")
        } else {
            let octets = announced.replace('.', ",");
            format!("PORT {octets},{},{}", port / 256, port % 256)
        };
        self.expect(&request)?;
        let reply = self.command(command)?;
        if !reply.is_positive() {
            return Err(reply_error(command, &reply));
        }
        let socket = accept_bounded(&listener, expected.as_deref(), deadline, cancel)?;
        let stream = PollStream::new(socket, deadline, cancel.clone(), "data channel")?;
        self.protect(Channel::Plain(stream))
    }

    /// Wrap a data channel when the session protects data.
    fn protect(&self, channel: Channel) -> VfsResult<Channel> {
        if !self.protect_data {
            return Ok(channel);
        }
        let Some(config) = self.tls.clone() else {
            return Err(VfsError::tls(
                "the data channel must be secured and no configuration was built",
            ));
        };
        let mut channel = channel.secure(&config, &self.host)?;
        // The control connection and this one share one session store, so this
        // handshake resumes the control connection's session where the server
        // offers one. The handshake runs here rather than on the first byte so
        // a server that refuses it is reported as a transport failure that
        // names the cause, not as a truncated transfer.
        channel.finish_handshake().map_err(|error| {
            VfsError::tls(format!(
                "the data channel handshake failed: {error}. A server that requires the data \
                 connection to resume the control connection's session refuses a new session here."
            ))
        })?;
        Ok(channel)
    }

    /// Ask the server for a data address.
    ///
    /// The address a passive reply names is discarded and the peer of the
    /// control connection is used instead. A server that names a third party
    /// would otherwise make this client open a data connection to that party
    /// and read or write the caller's content over it.
    fn passive_address(&mut self) -> VfsResult<(String, u16)> {
        if self.features.epsv {
            let reply = self.command("EPSV")?;
            if reply.is_positive() {
                if let Some(port) = parse_epsv(&reply.text) {
                    return Ok((self.data_host(), port));
                }
            }
        }
        let reply = self.expect("PASV")?;
        let (address, port) = parse_pasv(&reply.text).ok_or_else(|| {
            VfsError::protocol(format!(
                "the passive reply {:?} names no address",
                one_line(&reply.text)
            ))
        })?;
        if self.trust_passive_address {
            return Ok((address, port));
        }
        Ok((self.data_host(), port))
    }

    /// The host a data connection is opened to.
    fn data_host(&self) -> String {
        self.control_local_peer()
            .unwrap_or_else(|_| self.host.clone())
    }

    /// The socket under the control channel.
    fn control_socket(&self) -> VfsResult<&std::net::TcpStream> {
        self.control
            .socket()
            .ok_or_else(|| VfsError::network("the control channel is not connected"))
    }

    /// The local address of the control connection, as text.
    fn control_local_address(&self) -> VfsResult<String> {
        let socket = self.control_socket()?;
        socket
            .local_addr()
            .map(|address| address.ip().to_string())
            .map_err(|error| VfsError::network(error.to_string()))
    }

    /// The address the control connection reached, as text.
    fn control_local_peer(&self) -> VfsResult<String> {
        let socket = self.control_socket()?;
        socket
            .peer_addr()
            .map(|address| address.ip().to_string())
            .map_err(|error| VfsError::network(error.to_string()))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.send_raw("QUIT");
    }
}

/// Bind a listening socket for an active transfer.
///
/// The socket binds the one local address the control connection uses, so the
/// listening port is not offered on every interface of the machine.
fn bind_active(range: Option<(u16, u16)>, local: &str) -> VfsResult<TcpListener> {
    let address: std::net::IpAddr = local
        .parse()
        .map_err(|_| VfsError::network("the control connection has no usable local address"))?;
    match range {
        Some((first, last)) if first <= last => {
            for port in first..=last {
                if let Ok(listener) = TcpListener::bind((address, port)) {
                    return Ok(listener);
                }
            }
            Err(VfsError::network(format!(
                "no port between {first} and {last} could be opened for an active transfer"
            )))
        }
        _ => TcpListener::bind((address, 0)).map_err(|error| VfsError::network(error.to_string())),
    }
}

/// Wait for the server to open the data connection, within the deadline.
///
/// A connection from any address other than the server's is closed and the
/// wait continues, so a third party cannot take the transfer.
fn accept_bounded(
    listener: &TcpListener,
    expected: Option<&str>,
    deadline: Deadline,
    cancel: &Cancel,
) -> VfsResult<std::net::TcpStream> {
    listener
        .set_nonblocking(true)
        .map_err(|error| VfsError::network(error.to_string()))?;
    loop {
        cancel.check()?;
        deadline.check("data channel")?;
        match listener.accept() {
            Ok((socket, peer)) => {
                if expected.is_some_and(|address| address != peer.ip().to_string()) {
                    continue;
                }
                socket
                    .set_nonblocking(false)
                    .map_err(|error| VfsError::network(error.to_string()))?;
                return Ok(socket);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(VfsError::network(format!(
                    "the server did not connect: {error}"
                )))
            }
        }
    }
}

/// Three digit status at the head of a reply line.
fn status_of(line: &str) -> Option<u16> {
    let digits = line.get(..3)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The port of an extended passive reply.
#[must_use]
pub fn parse_epsv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = text.get(open..)?.find(')')? + open;
    let body = text.get(open + 1..close)?;
    let mut parts = body.split(body.chars().next()?);
    parts.next()?;
    parts.next()?;
    parts.next()?;
    parts.next()?.parse().ok()
}

/// The address and port of a passive reply.
#[must_use]
pub fn parse_pasv(text: &str) -> Option<(String, u16)> {
    let open = text.find('(')?;
    let close = text.get(open..)?.find(')')? + open;
    let body = text.get(open + 1..close)?;
    let numbers: Vec<u16> = body
        .split(',')
        .map(|part| part.trim().parse::<u16>().unwrap_or(u16::MAX))
        .collect();
    if numbers.len() != 6 || numbers.iter().any(|value| *value > 255) {
        return None;
    }
    let address = format!(
        "{}.{}.{}.{}",
        numbers.first()?,
        numbers.get(1)?,
        numbers.get(2)?,
        numbers.get(3)?
    );
    Some((address, numbers.get(4)? * 256 + numbers.get(5)?))
}

/// Build the error for a command the server refused.
fn reply_error(command: &str, reply: &Reply) -> VfsError {
    let verb = command.split_whitespace().next().unwrap_or("command");
    VfsError::protocol(format!("{verb} failed: {}", one_line(&reply.text)))
}

/// One line of a reply, bounded, for an error message.
#[must_use]
pub fn one_line(text: &str) -> String {
    let joined: String = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    trim(&joined)
}

/// Bound a string for an error message.
fn trim(text: &str) -> String {
    let mut out: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect();
    out = out.trim().to_owned();
    out
}

/// Read listing bytes as text, under the profile's encoding.
#[must_use]
pub fn decode(bytes: &[u8], encoding: &ListingEncoding) -> String {
    match encoding {
        ListingEncoding::Utf8 | ListingEncoding::Unknown(_) => {
            String::from_utf8_lossy(bytes).into_owned()
        }
        ListingEncoding::Latin1 => bytes.iter().map(|byte| char::from(*byte)).collect(),
        ListingEncoding::Detect => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => bytes.iter().map(|byte| char::from(*byte)).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn the_passive_replies_parse() {
        assert_eq!(
            parse_pasv("Entering Passive Mode (127,0,0,1,195,80)"),
            Some(("127.0.0.1".to_owned(), 50_000))
        );
        assert_eq!(parse_pasv("Entering Passive Mode (1,2,3)"), None);
        assert_eq!(
            parse_epsv("Entering Extended Passive Mode (|||50000|)"),
            Some(50_000)
        );
        assert_eq!(parse_epsv("no address here"), None);
    }

    #[test]
    fn the_feature_reply_parses() {
        let features = Features::parse(" MLSD\n MDTM\n SIZE\n REST STREAM\n UTF8\n");
        assert!(features.mlsd && features.mdtm && features.size && features.rest && features.utf8);
        assert!(!features.mfmt);
    }

    #[test]
    fn an_error_message_carries_no_control_characters() {
        let text = one_line("530 Login\u{7}incorrect\r\nsecond line");
        assert!(!text.contains('\u{7}'));
        assert!(text.contains("530 Loginincorrect"));
    }

    #[test]
    fn the_listing_encodings_decode() {
        assert_eq!(decode(b"caf\xc3\xa9", &ListingEncoding::Utf8), "café");
        assert_eq!(decode(b"caf\xe9", &ListingEncoding::Latin1), "café");
        assert_eq!(decode(b"caf\xe9", &ListingEncoding::Detect), "café");
    }
}
