//! A secure shell server with a transfer subsystem, running in this process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::keys::{PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};

use super::Stop;

/// A running server.
pub struct SshTestServer {
    port: u16,
    stop: Stop,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The key the server offers, in the encoding the store records.
    pub host_key: Vec<u8>,
    /// The algorithm name of that key.
    pub host_key_type: String,
}

impl SshTestServer {
    /// Start a server over `root` that accepts one account.
    pub fn start(root: &Path, username: &str, password: &str) -> Self {
        let key = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
            .expect("a key can be generated");
        let public = key.public_key().clone();
        let host_key = PublicKey::to_bytes(&public).expect("the key encodes");
        let host_key_type = public.algorithm().to_string();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime can be started");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .expect("a port can be opened");
        let port = listener.local_addr().expect("the port is known").port();

        let stop = Stop::new();
        let thread = {
            let stop = stop.clone();
            let root = root.to_path_buf();
            let username = username.to_owned();
            let password = password.to_owned();
            std::thread::spawn(move || {
                runtime.block_on(async move {
                    let config = Arc::new(russh::server::Config {
                        keys: vec![key],
                        inactivity_timeout: Some(std::time::Duration::from_secs(30)),
                        ..russh::server::Config::default()
                    });
                    let mut server = Server {
                        root,
                        username,
                        password,
                    };
                    loop {
                        if stop.raised() {
                            return;
                        }
                        let accepted = tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            listener.accept(),
                        )
                        .await;
                        let Ok(Ok((socket, _))) = accepted else {
                            continue;
                        };
                        let handler = russh::server::Server::new_client(&mut server, None);
                        let config = Arc::clone(&config);
                        tokio::spawn(async move {
                            let _ = russh::server::run_stream(config, socket, handler).await;
                        });
                    }
                });
            })
        };

        Self {
            port,
            stop,
            thread: Some(thread),
            host_key,
            host_key_type,
        }
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for SshTestServer {
    fn drop(&mut self) {
        self.stop.raise();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The server, which makes one handler for each client.
struct Server {
    root: PathBuf,
    username: String,
    password: String,
}

impl russh::server::Server for Server {
    type Handler = SshHandler;

    fn new_client(&mut self, _address: Option<std::net::SocketAddr>) -> Self::Handler {
        SshHandler {
            root: self.root.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            channels: HashMap::new(),
        }
    }
}

/// One client's session.
struct SshHandler {
    root: PathBuf,
    username: String,
    password: String,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Handler for SshHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == self.username && password == self.password {
            return Ok(Auth::Accept);
        }
        Ok(Auth::Reject {
            proceed_with_methods: None,
            partial_success: false,
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: None,
            partial_success: false,
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel_id)?;
            return Ok(());
        }
        let Some(channel) = self.channels.remove(&channel_id) else {
            session.channel_failure(channel_id)?;
            return Ok(());
        };
        session.channel_success(channel_id)?;
        let handler = SftpHandler::new(self.root.clone());
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), handler).await;
        });
        Ok(())
    }
}

/// The transfer subsystem, serving one folder.
struct SftpHandler {
    root: PathBuf,
    version: Option<u32>,
    /// Open files and folders, keyed by the handle the client is given.
    files: HashMap<String, PathBuf>,
    directories: HashMap<String, bool>,
    next: u64,
}

impl SftpHandler {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            version: None,
            files: HashMap::new(),
            directories: HashMap::new(),
            next: 0,
        }
    }

    /// The real path a request names, refusing anything that leaves the root.
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        let mut out = self.root.clone();
        for part in path.replace('\\', "/").split('/') {
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

    /// A handle no other open call has used.
    fn handle(&mut self) -> String {
        self.next += 1;
        format!("h{}", self.next)
    }
}

/// The attributes of one real entry.
fn attributes_of(path: &Path) -> FileAttributes {
    let Ok(metadata) = std::fs::metadata(path) else {
        return FileAttributes::default();
    };
    let mut attributes = FileAttributes {
        size: Some(metadata.len()),
        uid: Some(0),
        gid: Some(0),
        permissions: None,
        atime: None,
        mtime: Some(
            u32::try_from(crate::remote::timestamp::unix_seconds(
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
            ))
            .unwrap_or(0),
        ),
        user: None,
        group: None,
    };
    attributes.set_dir(metadata.is_dir());
    attributes.set_regular(metadata.is_file());
    let mode = if metadata.is_dir() { 0o755 } else { 0o644 };
    attributes.permissions = Some(attributes.permissions.unwrap_or(0) | mode);
    attributes
}

impl russh_sftp::server::Handler for SftpHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<russh_sftp::protocol::Version, Self::Error> {
        if self.version.is_some() {
            return Err(StatusCode::ConnectionLost);
        }
        self.version = Some(version);
        Ok(russh_sftp::protocol::Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let cleaned = if path == "." || path.is_empty() {
            "/".to_owned()
        } else {
            path
        };
        Ok(Name {
            id,
            files: vec![File::dummy(cleaned)],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let target = self.resolve(&path).ok_or(StatusCode::PermissionDenied)?;
        if !target.is_dir() {
            return Err(StatusCode::NoSuchFile);
        }
        let handle = self.handle();
        self.files.insert(handle.clone(), target);
        self.directories.insert(handle.clone(), false);
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        if self.directories.get(&handle).copied().unwrap_or(true) {
            return Err(StatusCode::Eof);
        }
        let target = self
            .files
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        self.directories.insert(handle, true);
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&target)
            .map_err(|_| StatusCode::Failure)?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let attrs = attributes_of(&entry.path());
            files.push(File {
                filename: name.clone(),
                longname: name,
                attrs,
            });
        }
        Ok(Name { id, files })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let target = self
            .resolve(&filename)
            .ok_or(StatusCode::PermissionDenied)?;
        if pflags.contains(OpenFlags::WRITE) {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if pflags.contains(OpenFlags::TRUNCATE) || !target.exists() {
                std::fs::write(&target, b"").map_err(|_| StatusCode::Failure)?;
            }
        } else if !target.is_file() {
            return Err(StatusCode::NoSuchFile);
        }
        let handle = self.handle();
        self.files.insert(handle.clone(), target);
        Ok(Handle { id, handle })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let target = self
            .files
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        let content = std::fs::read(&target).map_err(|_| StatusCode::Failure)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(content.len());
        if start >= content.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + usize::try_from(len).unwrap_or(0)).min(content.len());
        Ok(Data {
            id,
            data: content.get(start..end).unwrap_or_default().to_vec(),
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let target = self
            .files
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        let mut content = std::fs::read(&target).unwrap_or_default();
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if content.len() < start {
            content.resize(start, 0);
        }
        let end = start + data.len();
        if content.len() < end {
            content.resize(end, 0);
        }
        content
            .get_mut(start..end)
            .ok_or(StatusCode::Failure)?
            .copy_from_slice(&data);
        std::fs::write(&target, content).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.files.remove(&handle);
        self.directories.remove(&handle);
        Ok(ok(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let target = self.resolve(&path).ok_or(StatusCode::PermissionDenied)?;
        if !target.exists() {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Attrs {
            id,
            attrs: attributes_of(&target),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let target = self
            .files
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        Ok(Attrs {
            id,
            attrs: attributes_of(&target),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let target = self
            .resolve(&filename)
            .ok_or(StatusCode::PermissionDenied)?;
        std::fs::remove_file(target).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let target = self.resolve(&path).ok_or(StatusCode::PermissionDenied)?;
        std::fs::create_dir(target).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let target = self.resolve(&path).ok_or(StatusCode::PermissionDenied)?;
        std::fs::remove_dir(target).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let source = self.resolve(&oldpath).ok_or(StatusCode::PermissionDenied)?;
        let target = self.resolve(&newpath).ok_or(StatusCode::PermissionDenied)?;
        std::fs::rename(source, target).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }
}

/// A status saying the request completed.
fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "ok".to_owned(),
        language_tag: "en-US".to_owned(),
    }
}
