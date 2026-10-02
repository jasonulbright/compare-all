//! A file transfer server that runs in this process.
//!
//! It implements only what the tests exercise, which is what the client sends:
//! the login, the feature list, both listing commands, the size and time
//! queries, passive transfers, restart at an offset, upload, rename and
//! delete. Two extra modes let a test see what the client does with a server
//! that answers badly: a crafted listing, and a transfer that never finishes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Stop, TestCertificate};

/// How the server behaves.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Account the server accepts. `None` accepts any account.
    pub account: Option<(String, String)>,
    /// Offer the machine-readable listing.
    pub mlsd: bool,
    /// Secure the control channel from the first byte.
    pub implicit_tls: bool,
    /// Accept the command that secures the control channel.
    pub explicit_tls: bool,
    /// Answer every listing with a crafted one.
    pub hostile_listing: bool,
    /// Send one byte of a download per interval, so a test can cancel or time
    /// out part way.
    pub slow_transfer: Option<Duration>,
    /// Accept an upload and then close the data channel part way.
    pub break_uploads: bool,
    /// Accept the connection and never answer.
    pub silent: bool,
    /// Refuse a data connection whose handshake does not resume a session.
    pub require_data_resumption: bool,
    /// Refuse the command that protects the data channel.
    pub refuse_data_protection: bool,
    /// Refuse every rename.
    pub refuse_renames: bool,
}

/// A running server.
pub struct FtpTestServer {
    port: u16,
    root: PathBuf,
    stop: Stop,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The certificate the server offers, when it speaks transport security.
    pub certificate: Option<Arc<TestCertificate>>,
}

impl FtpTestServer {
    /// Start a server over `root`.
    pub fn start(root: &Path, options: Options) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Stop::new();
        let certificate = (options.implicit_tls || options.explicit_tls)
            .then(|| Arc::new(TestCertificate::new()));

        let thread = {
            let stop = stop.clone();
            let root = root.to_path_buf();
            let certificate = certificate.clone();
            std::thread::spawn(move || {
                while !stop.raised() {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let options = options.clone();
                            let root = root.clone();
                            let certificate = certificate.clone();
                            let stop = stop.clone();
                            std::thread::spawn(move || {
                                let _ =
                                    serve(socket, &root, &options, certificate.as_deref(), &stop);
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
            root: root.to_path_buf(),
            stop,
            thread: Some(thread),
            certificate,
        }
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The folder the server serves.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for FtpTestServer {
    fn drop(&mut self) {
        self.stop.raise();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A control channel, with or without transport security.
enum Channel {
    Plain(TcpStream),
    Secure(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Channel {
    fn secure(self, config: &Arc<rustls::ServerConfig>) -> std::io::Result<Self> {
        let stream = match self {
            Self::Plain(stream) => stream,
            other => return Ok(other),
        };
        let connection = rustls::ServerConnection::new(Arc::clone(config))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self::Secure(Box::new(rustls::StreamOwned::new(
            connection, stream,
        ))))
    }
}

impl Channel {
    /// Run the handshake to completion and report whether it resumed a stored
    /// session.
    fn finish_handshake(&mut self) -> std::io::Result<bool> {
        let Self::Secure(stream) = self else {
            return Ok(false);
        };
        while stream.conn.is_handshaking() {
            let (read, written) = stream.conn.complete_io(&mut stream.sock)?;
            if read == 0 && written == 0 {
                return Err(std::io::Error::other("the handshake stalled"));
            }
        }
        Ok(stream.conn.handshake_kind() == Some(rustls::HandshakeKind::Resumed))
    }
}

impl Channel {
    /// End the channel the way the protocol expects, so the other side sees a
    /// clean end of stream rather than a broken connection.
    fn close(mut self) {
        if let Self::Secure(stream) = &mut self {
            stream.conn.send_close_notify();
            let _ = stream.flush();
        }
    }
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

/// State of one connected client.
struct Session {
    root: PathBuf,
    working: String,
    rename_from: Option<PathBuf>,
    restart: u64,
    authenticated: bool,
    pending_user: Option<String>,
    data: Option<TcpListener>,
    protect_data: bool,
}

/// Serve one client until it quits.
fn serve(
    socket: TcpStream,
    root: &Path,
    options: &Options,
    certificate: Option<&TestCertificate>,
    stop: &Stop,
) -> std::io::Result<()> {
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    if options.silent {
        while !stop.raised() {
            std::thread::sleep(Duration::from_millis(20));
        }
        return Ok(());
    }

    let config = certificate.map(TestCertificate::server_config);
    let mut channel = Channel::Plain(socket);
    if options.implicit_tls {
        if let Some(config) = &config {
            channel = channel.secure(config)?;
        }
    }

    let mut session = Session {
        root: root.to_path_buf(),
        working: "/".to_owned(),
        rename_from: None,
        restart: 0,
        authenticated: options.account.is_none(),
        pending_user: None,
        data: None,
        protect_data: false,
    };
    reply(&mut channel, "220 ready")?;

    let shared = Arc::new(Mutex::new(()));
    loop {
        if stop.raised() {
            return Ok(());
        }
        let Some(line) = read_command(&mut channel)? else {
            return Ok(());
        };
        let (verb, argument) = split(&line);
        let verb = verb.to_ascii_uppercase();
        let _guard = shared.lock();

        match verb.as_str() {
            "QUIT" => {
                reply(&mut channel, "221 bye")?;
                return Ok(());
            }
            "AUTH" if options.explicit_tls && argument.eq_ignore_ascii_case("TLS") => {
                reply(&mut channel, "234 go ahead")?;
                if let Some(config) = &config {
                    channel = channel.secure(config)?;
                }
            }
            "AUTH" => reply(&mut channel, "500 not understood")?,
            "USER" => {
                session.pending_user = Some(argument.to_owned());
                if options.account.is_none() {
                    session.authenticated = true;
                    reply(&mut channel, "230 logged in")?;
                } else {
                    reply(&mut channel, "331 need a password")?;
                }
            }
            "PASS" => {
                let wanted = options.account.clone();
                let user = session.pending_user.clone().unwrap_or_default();
                match wanted {
                    Some((name, secret)) if name == user && secret == argument => {
                        session.authenticated = true;
                        reply(&mut channel, "230 logged in")?;
                    }
                    Some(_) => reply(&mut channel, "530 login incorrect")?,
                    None => {
                        session.authenticated = true;
                        reply(&mut channel, "230 logged in")?;
                    }
                }
            }
            "FEAT" => {
                let mut text = String::from(
                    "211-features\r\n SIZE\r\n MDTM\r\n MFMT\r\n REST STREAM\r\n UTF8\r\n",
                );
                if options.mlsd {
                    text.push_str(" MLSD\r\n");
                }
                text.push_str("211 end");
                reply(&mut channel, &text)?;
            }
            "OPTS" | "NOOP" => reply(&mut channel, "200 ok")?,
            _ if !session.authenticated => reply(&mut channel, "530 log in first")?,
            "TYPE" | "PBSZ" => reply(&mut channel, "200 ok")?,
            "PROT" if options.refuse_data_protection => {
                reply(&mut channel, "534 protection refused")?;
            }
            "PROT" => {
                session.protect_data = argument.eq_ignore_ascii_case("P");
                reply(&mut channel, "200 ok")?;
            }
            "SYST" => reply(&mut channel, "215 UNIX Type: L8")?,
            "PWD" => {
                let working = session.working.clone();
                reply(&mut channel, &format!("257 \"{working}\" is current"))?;
            }
            "CWD" => {
                let target = resolve(&session, argument);
                if target.is_dir() {
                    session.working = absolute(&session, argument);
                    reply(&mut channel, "250 ok")?;
                } else {
                    reply(&mut channel, "550 no such folder")?;
                }
            }
            "PASV" => {
                let listener = TcpListener::bind(("127.0.0.1", 0))?;
                let port = listener.local_addr()?.port();
                session.data = Some(listener);
                reply(
                    &mut channel,
                    &format!(
                        "227 Entering Passive Mode (127,0,0,1,{},{})",
                        port / 256,
                        port % 256
                    ),
                )?;
            }
            "EPSV" => {
                let listener = TcpListener::bind(("127.0.0.1", 0))?;
                let port = listener.local_addr()?.port();
                session.data = Some(listener);
                reply(
                    &mut channel,
                    &format!("229 Entering Extended Passive Mode (|||{port}|)"),
                )?;
            }
            "LIST" | "MLSD" => {
                let machine = verb == "MLSD";
                if machine && !options.mlsd {
                    reply(&mut channel, "500 not understood")?;
                    continue;
                }
                let target = resolve(&session, strip_flags(argument));
                let body = if options.hostile_listing {
                    hostile_body(machine)
                } else {
                    match listing(&target, machine) {
                        Some(body) => body,
                        None => {
                            reply(&mut channel, "550 no such folder")?;
                            continue;
                        }
                    }
                };
                reply(&mut channel, "150 opening data channel")?;
                send_data(&mut session, body.as_bytes(), &config, options, stop)?;
                reply(&mut channel, "226 transfer complete")?;
            }
            "SIZE" => {
                let target = resolve(&session, argument);
                match std::fs::metadata(&target) {
                    Ok(meta) if meta.is_file() => {
                        reply(&mut channel, &format!("213 {}", meta.len()))?;
                    }
                    _ => reply(&mut channel, "550 no such file")?,
                }
            }
            "MDTM" => {
                let target = resolve(&session, argument);
                match modified_of(&target) {
                    Some(seconds) => {
                        let stamp = crate::remote::timestamp::format_ftp_stamp(seconds);
                        reply(&mut channel, &format!("213 {stamp}"))?;
                    }
                    None => reply(&mut channel, "550 no such file")?,
                }
            }
            "MFMT" => {
                let (_stamp, rest) = split(argument);
                let target = resolve(&session, rest);
                if target.exists() {
                    reply(&mut channel, "213 modify=ok")?;
                } else {
                    reply(&mut channel, "550 no such file")?;
                }
            }
            "REST" => {
                session.restart = argument.trim().parse().unwrap_or(0);
                reply(&mut channel, "350 restarting")?;
            }
            "RETR" => {
                let target = resolve(&session, argument);
                let Ok(content) = std::fs::read(&target) else {
                    reply(&mut channel, "550 no such file")?;
                    continue;
                };
                let offset = usize::try_from(session.restart)
                    .unwrap_or(0)
                    .min(content.len());
                session.restart = 0;
                reply(&mut channel, "150 opening data channel")?;
                send_data(
                    &mut session,
                    content.get(offset..).unwrap_or_default(),
                    &config,
                    options,
                    stop,
                )?;
                reply(&mut channel, "226 transfer complete")?;
            }
            "STOR" => {
                let target = resolve(&session, argument);
                reply(&mut channel, "150 opening data channel")?;
                match receive_data(&mut session, &config, options, stop) {
                    Ok(bytes) if !options.break_uploads => {
                        if let Some(parent) = target.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        std::fs::write(&target, bytes)?;
                        reply(&mut channel, "226 transfer complete")?;
                    }
                    _ => reply(&mut channel, "426 transfer failed")?,
                }
            }
            "DELE" => {
                let target = resolve(&session, argument);
                if std::fs::remove_file(&target).is_ok() {
                    reply(&mut channel, "250 deleted")?;
                } else {
                    reply(&mut channel, "550 no such file")?;
                }
            }
            "MKD" => {
                let target = resolve(&session, argument);
                if std::fs::create_dir(&target).is_ok() {
                    reply(&mut channel, "257 created")?;
                } else {
                    reply(&mut channel, "550 not created")?;
                }
            }
            "RMD" => {
                let target = resolve(&session, argument);
                if std::fs::remove_dir(&target).is_ok() {
                    reply(&mut channel, "250 removed")?;
                } else {
                    reply(&mut channel, "550 not removed")?;
                }
            }
            "RNFR" => {
                let target = resolve(&session, argument);
                if target.exists() {
                    session.rename_from = Some(target);
                    reply(&mut channel, "350 ready")?;
                } else {
                    reply(&mut channel, "550 no such file")?;
                }
            }
            "RNTO" if options.refuse_renames => {
                session.rename_from = None;
                reply(&mut channel, "553 rename refused")?;
            }
            "RNTO" => {
                let target = resolve(&session, argument);
                match session.rename_from.take() {
                    Some(source) if std::fs::rename(&source, &target).is_ok() => {
                        reply(&mut channel, "250 renamed")?;
                    }
                    _ => reply(&mut channel, "550 not renamed")?,
                }
            }
            _ => reply(&mut channel, "502 not implemented")?,
        }
    }
}

/// Write one reply.
fn reply(channel: &mut Channel, text: &str) -> std::io::Result<()> {
    channel.write_all(format!("{text}\r\n").as_bytes())?;
    channel.flush()
}

/// Read one command line, waiting through the read timeout.
fn read_command(channel: &mut Channel) -> std::io::Result<Option<String>> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    let mut idle = 0u32;
    loop {
        match channel.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                if byte[0] == b'\n' {
                    while out.last() == Some(&b'\r') {
                        out.pop();
                    }
                    return Ok(Some(String::from_utf8_lossy(&out).into_owned()));
                }
                out.push(byte[0]);
                if out.len() > 8192 {
                    return Ok(None);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                idle += 1;
                if idle > 60 {
                    return Ok(None);
                }
            }
            Err(_) => return Ok(None),
        }
    }
}

/// Split a line into its first word and the rest.
fn split(line: &str) -> (&str, &str) {
    match line.trim().split_once(' ') {
        Some((head, tail)) => (head, tail.trim()),
        None => (line.trim(), ""),
    }
}

/// Drop the option letters a listing command may carry.
fn strip_flags(argument: &str) -> &str {
    let mut rest = argument.trim();
    while let Some(tail) = rest.strip_prefix('-') {
        rest = tail.split_once(' ').map_or("", |(_, value)| value.trim());
    }
    rest
}

/// The absolute path a command names.
fn absolute(session: &Session, argument: &str) -> String {
    let argument = argument.trim();
    if argument.is_empty() {
        return session.working.clone();
    }
    if argument.starts_with('/') {
        return argument.trim_end_matches('/').to_owned();
    }
    format!("{}/{argument}", session.working.trim_end_matches('/'))
}

/// The real path a command names.
fn resolve(session: &Session, argument: &str) -> PathBuf {
    let path = absolute(session, argument);
    let mut out = session.root.clone();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        out.push(part);
    }
    out
}

/// The modification time of a file, in seconds from the Unix epoch.
fn modified_of(path: &Path) -> Option<i64> {
    let metadata = std::fs::metadata(path).ok()?;
    let time = metadata.modified().ok()?;
    Some(crate::remote::timestamp::unix_seconds(time))
}

/// The listing of one folder.
fn listing(target: &Path, machine: bool) -> Option<String> {
    let entries = std::fs::read_dir(target).ok()?;
    let mut out = String::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = entry.metadata().ok()?;
        let seconds = metadata
            .modified()
            .ok()
            .map_or(0, crate::remote::timestamp::unix_seconds);
        if machine {
            let kind = if metadata.is_dir() { "dir" } else { "file" };
            let stamp = crate::remote::timestamp::format_ftp_stamp(seconds);
            out.push_str(&format!(
                "type={kind};size={};modify={stamp};unix.mode=0644; {name}\r\n",
                metadata.len()
            ));
        } else {
            let (year, month, day, hour, minute, _) =
                crate::remote::timestamp::unix_to_civil(seconds);
            let name_of_month = crate::remote::timestamp::month_name(month);
            let prefix = if metadata.is_dir() {
                "drwxr-xr-x"
            } else {
                "-rw-r--r--"
            };
            let _ = year;
            out.push_str(&format!(
                "{prefix}   1 owner group {:>12} {name_of_month} {day:>2} {hour:02}:{minute:02} \
                 {name}\r\n",
                metadata.len()
            ));
        }
    }
    Some(out)
}

/// A listing built to break a client that trusts it.
fn hostile_body(machine: bool) -> String {
    let names = [
        "../escape.txt",
        "/etc/passwd",
        "..",
        ".",
        "with\u{7}control.txt",
        "Report.TXT",
        "report.txt",
        "trailing.",
        "C:/Windows/system.ini",
        "ordinary.txt",
    ];
    let mut out = String::new();
    for name in names {
        if machine {
            out.push_str(&format!(
                "type=file;size=10;modify=20240102030405; {name}\r\n"
            ));
        } else {
            out.push_str(&format!(
                "-rw-r--r--   1 owner group           10 Jan  2 03:04 {name}\r\n"
            ));
        }
    }
    out
}

/// Accept the data connection.
fn accept_data(session: &mut Session) -> std::io::Result<TcpStream> {
    let listener = session
        .data
        .take()
        .ok_or_else(|| std::io::Error::other("no data channel was opened"))?;
    listener.set_nonblocking(false)?;
    let (socket, _) = listener.accept()?;
    Ok(socket)
}

/// Complete the data channel handshake, applying the resumption demand.
fn secure_data_channel(channel: &mut Channel, options: &Options) -> std::io::Result<()> {
    let resumed = channel.finish_handshake()?;
    if options.require_data_resumption && !resumed {
        return Err(std::io::Error::other(
            "the data connection did not resume the control connection's session",
        ));
    }
    Ok(())
}

/// Write a body on the data channel.
fn send_data(
    session: &mut Session,
    body: &[u8],
    config: &Option<Arc<rustls::ServerConfig>>,
    options: &Options,
    stop: &Stop,
) -> std::io::Result<()> {
    let socket = accept_data(session)?;
    let mut channel = Channel::Plain(socket);
    if session.protect_data {
        if let Some(config) = config {
            channel = channel.secure(config)?;
            secure_data_channel(&mut channel, options)?;
        }
    }
    match options.slow_transfer {
        Some(delay) => {
            for byte in body {
                if stop.raised() {
                    break;
                }
                if channel.write_all(&[*byte]).is_err() {
                    break;
                }
                let _ = channel.flush();
                std::thread::sleep(delay);
            }
        }
        None => channel.write_all(body)?,
    }
    let _ = channel.flush();
    channel.close();
    Ok(())
}

/// Read a body from the data channel.
fn receive_data(
    session: &mut Session,
    config: &Option<Arc<rustls::ServerConfig>>,
    options: &Options,
    _stop: &Stop,
) -> std::io::Result<Vec<u8>> {
    let socket = accept_data(session)?;
    let mut channel = Channel::Plain(socket);
    if session.protect_data {
        if let Some(config) = config {
            channel = channel.secure(config)?;
            secure_data_channel(&mut channel, options)?;
        }
    }
    if options.break_uploads {
        let mut small = [0u8; 8];
        let _ = channel.read(&mut small);
        return Err(std::io::Error::other("the upload was cut short"));
    }
    let mut out = Vec::new();
    let mut reader = BufReader::new(channel);
    loop {
        let read = {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            out.extend_from_slice(buffer);
            buffer.len()
        };
        reader.consume(read);
    }
    Ok(out)
}
