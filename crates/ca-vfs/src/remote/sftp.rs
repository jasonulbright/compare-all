//! The secure shell transfer protocol.
//!
//! The library that speaks the secure shell is written against an
//! asynchronous runtime. One current-thread runtime is created here and kept
//! inside this module, so the trait stays blocking and no asynchronous type
//! reaches a caller.
//!
//! The host key is checked against the store the caller names. A host that is
//! not recorded fails with [`VfsError::UnknownHostKey`] carrying the
//! fingerprint, so the interface can show it and ask; a recorded key that
//! differs fails with [`VfsError::HostKeyChanged`] and is never accepted.
//!
//! The protocol states a time in seconds in UTC and a size as an exact byte
//! count, so both are exact.

use std::io::{Read, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::fs::File;
use russh_sftp::client::{Config as SftpConfig, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::runtime::Runtime;

use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{VfsError, VfsResult};
use crate::fs::{Capabilities, FileSystem, OpenFile};
use crate::path::VfsPath;
use crate::remote::hostkeys::{host_label, ssh_fingerprint, KnownHosts};
use crate::remote::profile::FtpProfile;
use crate::remote::{child_path, nonce, temporary_name, RemoteContext, Secret};

/// How many bytes one read of an open file asks for.
const CHUNK: usize = 64 * 1024;
/// Largest SFTP response packet accepted from a server.
const MAX_SFTP_PACKET_BYTES: u32 = 16 * 1024 * 1024;

/// Rejects a server-declared SFTP packet before the library allocates its body.
struct BoundedSftpStream<S> {
    inner: S,
    max_packet_len: u32,
    packet_limit_hit: Arc<AtomicBool>,
    header: [u8; 4],
    header_len: usize,
    body_remaining: u32,
}

impl<S> BoundedSftpStream<S> {
    fn new(inner: S, max_packet_len: u32, packet_limit_hit: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            max_packet_len,
            packet_limit_hit,
            header: [0; 4],
            header_len: 0,
            body_remaining: 0,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for BoundedSftpStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.as_mut().get_mut();
        let before = buffer.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buffer))?;
        let mut oversized_prefix_end = None;
        {
            let filled = buffer.filled();
            for (offset, byte) in filled.get(before..).unwrap_or_default().iter().enumerate() {
                if this.body_remaining > 0 {
                    this.body_remaining -= 1;
                    continue;
                }
                if let Some(slot) = this.header.get_mut(this.header_len) {
                    *slot = *byte;
                }
                this.header_len += 1;
                if this.header_len == this.header.len() {
                    let declared = u32::from_be_bytes(this.header);
                    this.header_len = 0;
                    if declared > this.max_packet_len {
                        this.packet_limit_hit.store(true, Ordering::Release);
                        oversized_prefix_end = Some(before + offset + 1);
                        break;
                    }
                    this.body_remaining = declared;
                }
            }
        }
        if let Some(filled) = oversized_prefix_end {
            buffer.set_filled(filled);
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SFTP response packet exceeds the client limit",
            )));
        }
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for BoundedSftpStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.as_mut().get_mut().inner).poll_write(cx, bytes)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.as_mut().get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.as_mut().get_mut().inner).poll_shutdown(cx)
    }
}

/// The runtime, the transport and the transfer session, shared by the file
/// system and by every handle it hands out.
struct Core {
    runtime: Runtime,
    session: SftpSession,
    /// Kept so the transport outlives the session that runs over it.
    _handle: Handle<SshHandler>,
    /// Serializes the calls into the runtime, which is single threaded.
    lock: Mutex<()>,
    timeout: Duration,
    /// Remembers when the bounded transport rejected a server-declared packet.
    packet_limit_hit: Arc<AtomicBool>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Core")
    }
}

impl Core {
    /// Run one call on the runtime, bounded by the call timeout.
    fn run<T, F>(&self, operation: &'static str, work: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = Result<T, russh_sftp::client::error::Error>>,
    {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| VfsError::network("the session lock is poisoned"))?;
        let timeout = self.timeout;
        let result = self.runtime.block_on(async move {
            match tokio::time::timeout(timeout, work).await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(map_error(operation, &error)),
                Err(_) => Err(VfsError::timeout(operation.to_owned())),
            }
        });
        prioritize_packet_limit(&self.packet_limit_hit, result)
    }

    /// Run one call whose failure is an ordinary input or output failure.
    fn run_io<T, F>(&self, operation: &'static str, work: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = std::io::Result<T>>,
    {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| VfsError::network("the session lock is poisoned"))?;
        let timeout = self.timeout;
        let result = self.runtime.block_on(async move {
            match tokio::time::timeout(timeout, work).await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(VfsError::protocol(format!("{operation} failed: {error}"))),
                Err(_) => Err(VfsError::timeout(operation.to_owned())),
            }
        });
        prioritize_packet_limit(&self.packet_limit_hit, result)
    }
}

/// A file system backed by a secure shell server.
#[derive(Debug)]
pub struct SftpFs {
    core: Arc<Core>,
    base: String,
    label: String,
    context: RemoteContext,
}

/// What the client does with the server's key.
struct SshHandler {
    known_hosts: Option<KnownHosts>,
    host: String,
    port: u16,
    /// Set when the key was refused, so the blocking side reports the reason
    /// the library's own error does not carry.
    refusal: Arc<Mutex<Option<VfsError>>>,
}

impl SshHandler {
    /// Record why the key was refused.
    fn refuse(&self, error: VfsError) {
        let mut slot = self
            .refusal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(error);
    }
}

impl client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // A certificate host key would be trusted through its signing
        // authority, which this store does not hold, so it is refused rather
        // than reduced to the key it carries.
        let PublicKeyOrCertificate::PublicKey {
            key: server_key, ..
        } = server_key
        else {
            self.refuse(VfsError::unsupported(
                "the server offered a certificate host key, which is not checked here",
            ));
            return Ok(false);
        };
        let bytes = russh::keys::ssh_key::PublicKey::to_bytes(server_key).unwrap_or_default();
        let Some(store) = &self.known_hosts else {
            self.refuse(VfsError::UnknownHostKey {
                host: host_label(&self.host, self.port),
                fingerprint: ssh_fingerprint(&bytes),
            });
            return Ok(false);
        };
        let key_type = server_key.algorithm().to_string();
        match store.verify(&self.host, self.port, &key_type, &bytes) {
            Ok(()) => Ok(true),
            Err(error) => {
                self.refuse(error);
                Ok(false)
            }
        }
    }
}

impl SftpFs {
    /// Log in and open a transfer session.
    ///
    /// # Errors
    /// Returns [`VfsError::UnknownHostKey`] or [`VfsError::HostKeyChanged`]
    /// when the host key does not check out, [`VfsError::AuthFailed`] when the
    /// server refuses the account, and [`VfsError::Network`] otherwise. No
    /// secret reaches any message.
    #[allow(
        clippy::too_many_lines,
        reason = "one login sequence, written in the order the protocol runs it"
    )]
    pub fn connect(
        settings: &FtpProfile,
        context: &RemoteContext,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        cancel.check()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| VfsError::network(format!("no runtime could be started: {error}")))?;
        let packet_limit_hit = Arc::new(AtomicBool::new(false));

        let host = settings.login.host.clone();
        let port = settings
            .login
            .port
            .unwrap_or_else(|| settings.login.protocol.default_port());
        let store = if settings.global.known_hosts_file.trim().is_empty() {
            context.known_hosts.clone()
        } else {
            Some(KnownHosts::new(settings.global.known_hosts_file.trim()))
        };
        let refusal = Arc::new(Mutex::new(None));
        let handler = SshHandler {
            known_hosts: store,
            host: host.clone(),
            port,
            refusal: Arc::clone(&refusal),
        };

        let config = Arc::new(client::Config {
            inactivity_timeout: Some(context.call_timeout),
            ..client::Config::default()
        });
        let password = context.secret(&settings.login.password);
        let passphrase = context.secret(&settings.global.ssh_private_key_passphrase);
        let username = if settings.login.username.is_empty() {
            "anonymous".to_owned()
        } else {
            settings.login.username.clone()
        };
        let key_file = settings.global.ssh_private_key_file.trim().to_owned();
        let interactive = settings.global.ssh_keyboard_interactive;
        let timeout = context.call_timeout;

        let (handle, session) = runtime.block_on(async {
            let connected = tokio::time::timeout(
                timeout,
                client::connect(Arc::clone(&config), (host.as_str(), port), handler),
            )
            .await
            .map_err(|_| VfsError::timeout("connect".to_owned()))?;
            let mut handle = match connected {
                Ok(handle) => handle,
                Err(error) => {
                    if let Some(reason) = refusal
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        return Err(reason);
                    }
                    return Err(VfsError::network(format!(
                        "the server could not be reached: {error}"
                    )));
                }
            };

            let mut authenticated = false;
            if !key_file.is_empty() {
                let loaded = russh::keys::load_secret_key(
                    &key_file,
                    passphrase.as_ref().map(Secret::expose),
                )
                .map_err(|error| {
                    VfsError::auth_failed(format!("the private key could not be read: {error}"))
                })?;
                let algorithm = handle
                    .best_supported_rsa_hash()
                    .await
                    .ok()
                    .flatten()
                    .flatten();
                let result = handle
                    .authenticate_publickey(
                        &username,
                        PrivateKeyWithHashAlg::new(Arc::new(loaded), algorithm),
                    )
                    .await
                    .map_err(|error| VfsError::auth_failed(error.to_string()))?;
                authenticated = result.success();
            }
            if !authenticated {
                if let Some(secret) = &password {
                    let result = handle
                        .authenticate_password(&username, secret.expose())
                        .await
                        .map_err(|error| VfsError::auth_failed(error.to_string()))?;
                    authenticated = result.success();
                }
            }
            if !authenticated && interactive && password.is_none() {
                let result = handle
                    .authenticate_keyboard_interactive_start(&username, None)
                    .await
                    .map_err(|error| VfsError::auth_failed(error.to_string()))?;
                authenticated = matches!(result, client::KeyboardInteractiveAuthResponse::Success);
            }
            if !authenticated {
                return Err(VfsError::auth_failed(format!(
                    "the server refused the account {username:?}"
                )));
            }

            let channel = handle
                .channel_open_session()
                .await
                .map_err(|error| VfsError::network(error.to_string()))?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|error| VfsError::network(error.to_string()))?;
            let session = SftpSession::new_with_config(
                BoundedSftpStream::new(
                    channel.into_stream(),
                    MAX_SFTP_PACKET_BYTES,
                    packet_limit_hit.clone(),
                ),
                SftpConfig {
                    max_packet_len: MAX_SFTP_PACKET_BYTES,
                    ..SftpConfig::default()
                },
            )
            .await
            .map_err(|error| {
                if packet_limit_hit.load(Ordering::Acquire) {
                    packet_limit_error()
                } else {
                    VfsError::network(error.to_string())
                }
            })?;
            Ok((handle, session))
        })?;

        let base = if settings.root_path.trim().is_empty() {
            runtime
                .block_on(session.canonicalize("."))
                .unwrap_or_else(|_| ".".to_owned())
        } else {
            settings
                .root_path
                .trim()
                .replace('\\', "/")
                .trim_end_matches('/')
                .to_owned()
        };

        Ok(Self {
            core: Arc::new(Core {
                runtime,
                session,
                _handle: handle,
                lock: Mutex::new(()),
                timeout,
                packet_limit_hit,
            }),
            base,
            label: format!("sftp://{host}:{port}"),
            context: context.clone(),
        })
    }

    /// The server path for a path inside this file system.
    fn remote(&self, path: &VfsPath) -> String {
        let base = self.base.trim_end_matches('/');
        if path.is_root() {
            if base.is_empty() {
                "/".to_owned()
            } else {
                base.to_owned()
            }
        } else {
            format!("{base}/{path}")
        }
    }

    /// Open a read at `offset`.
    ///
    /// # Errors
    /// Returns whatever the server reports.
    pub fn open_at(&self, path: &VfsPath, offset: u64, cancel: &Cancel) -> VfsResult<OpenFile> {
        cancel.check()?;
        let remote = self.remote(path);
        let size = self
            .core
            .run("stat", self.core.session.metadata(remote.clone()))
            .ok()
            .and_then(|meta| meta.size);
        let mut handle = self.core.run(
            "open",
            self.core.session.open_with_flags(remote, OpenFlags::READ),
        )?;
        if offset > 0 {
            self.core
                .run_io("seek", handle.seek(SeekFrom::Start(offset)))?;
        }
        let length = size.map(|value| value.saturating_sub(offset));
        Ok(OpenFile::streaming(
            SftpRead {
                core: Arc::clone(&self.core),
                handle,
                cancel: cancel.clone(),
                done: false,
            },
            length,
        ))
    }

    /// Set the modification time of a file the server holds.
    ///
    /// # Errors
    /// Returns whatever the server reports.
    pub fn set_modified(&self, path: &VfsPath, seconds: i64) -> VfsResult<()> {
        let stamp = u32::try_from(seconds.max(0)).unwrap_or(u32::MAX);
        let attributes = FileAttributes {
            atime: Some(stamp),
            mtime: Some(stamp),
            ..FileAttributes::default()
        };
        self.core.run(
            "set time",
            self.core
                .session
                .set_metadata(self.remote(path), attributes),
        )
    }

    /// The ceilings one read is charged against.
    #[must_use]
    pub const fn limits(&self) -> &crate::limits::Limits {
        &self.context.limits
    }

    /// Delete everything under `path`, then `path` itself.
    fn delete_tree(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        for entry in self.list(path, cancel)? {
            cancel.check()?;
            if entry.is_dir() && !entry.is_link() {
                self.delete_tree(&entry.path, cancel)?;
            } else {
                self.core.run(
                    "delete",
                    self.core.session.remove_file(self.remote(&entry.path)),
                )?;
            }
        }
        self.core
            .run("delete", self.core.session.remove_dir(self.remote(path)))
    }
}

/// A read that owns its share of the session.
struct SftpRead {
    core: Arc<Core>,
    handle: File,
    cancel: Cancel,
    done: bool,
}

impl Read for SftpRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(crate::limits::carry(VfsError::Cancelled));
        }
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min(CHUNK);
        let Some(slice) = buf.get_mut(..want) else {
            return Ok(0);
        };
        let read = self
            .core
            .run_io("read", self.handle.read(slice))
            .map_err(crate::limits::carry)?;
        if read == 0 {
            self.done = true;
        }
        Ok(read)
    }
}

/// Turn a library error into a crate one.
fn map_error(operation: &str, error: &russh_sftp::client::error::Error) -> VfsError {
    if let russh_sftp::client::error::Error::Limited(resource) = error {
        return VfsError::ResourceLimit {
            resource: format!("{operation}: {resource}"),
        };
    }
    let text = error.to_string();
    let lowered = text.to_lowercase();
    if lowered.contains("no such file") {
        return VfsError::protocol(format!("{operation} found nothing"));
    }
    if lowered.contains("permission") {
        return VfsError::auth_failed(format!("{operation} was refused: {text}"));
    }
    VfsError::protocol(format!("{operation} failed: {text}"))
}

fn packet_limit_error() -> VfsError {
    VfsError::ResourceLimit {
        resource: format!("SFTP response packet exceeds {MAX_SFTP_PACKET_BYTES} bytes"),
    }
}

fn prioritize_packet_limit<T>(packet_limit_hit: &AtomicBool, result: VfsResult<T>) -> VfsResult<T> {
    if packet_limit_hit.load(Ordering::Acquire) {
        Err(packet_limit_error())
    } else {
        result
    }
}

/// Build one entry from the attributes the server reported.
fn to_entry(path: VfsPath, name: &str, attributes: &FileAttributes) -> VfsEntry {
    let is_dir = attributes.is_dir();
    let link = attributes.is_symlink().then_some(if is_dir {
        VfsLinkKind::DirectoryLink
    } else {
        VfsLinkKind::FileLink
    });
    VfsEntry {
        path,
        name: name.to_owned(),
        kind: if is_dir {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: attributes.size.unwrap_or(0),
        size_is_exact: true,
        modified: attributes
            .mtime
            .map(|value| crate::remote::timestamp::system_time(i64::from(value))),
        time_fidelity: TimeFidelity::Utc,
        created: None,
        attributes: attributes.permissions.map(|mode| VfsAttributes {
            read_only: mode & 0o200 == 0,
            hidden: name.starts_with('.'),
            system: false,
            archive: false,
            windows_bits: None,
            unix_mode: Some(mode),
            uid: attributes.uid,
            gid: attributes.gid,
        }),
        crc32: None,
        link,
        version_info: None,
        error: None,
        refused: false,
    }
}

impl FileSystem for SftpFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: true,
            supports_timestamps: true,
            supports_attributes: true,
            stored_crc: false,
            random_access: false,
            content_available: true,
        }
    }

    fn root_label(&self) -> String {
        format!("{}{}", self.label, self.base)
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        cancel.check()?;
        let listed = self
            .core
            .run("list", self.core.session.read_dir(self.remote(dir)))?;
        let mut out = Vec::new();
        let mut taken = crate::remote::ListedNames::default();
        for item in listed {
            cancel.check()?;
            let raw = item.file_name();
            if raw == "." || raw == ".." {
                continue;
            }
            let mut reason = None;
            let cleaned = sanitize(&raw);
            if cleaned != raw {
                reason = Some(format!(
                    "the server listed a name that no file system can hold: {}",
                    escape(&raw)
                ));
            }
            let (name, repeated) = taken.claim(cleaned);
            if repeated {
                reason.get_or_insert_with(|| {
                    format!(
                        "the server listed {} twice, or twice with the same letters in a \
                         different case",
                        escape(&raw)
                    )
                });
            }
            let Ok(path) = child_path(dir, &name) else {
                continue;
            };
            let mut entry = to_entry(path, &name, &item.metadata());
            entry.refused = reason.is_some();
            entry.error = reason;
            out.push(entry);
        }
        Ok(out)
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        if path.is_root() {
            return Ok(VfsEntry::directory(path.clone()));
        }
        let attributes = self
            .core
            .run("stat", self.core.session.metadata(self.remote(path)))
            .map_err(|_| VfsError::NotFound { path: path.clone() })?;
        let name = path.name().unwrap_or_default().to_owned();
        Ok(to_entry(path.clone(), &name, &attributes))
    }

    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile> {
        self.open_at(path, 0, cancel)
    }

    fn create_dir(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let mut walked = VfsPath::root();
        for component in path.components() {
            cancel.check()?;
            walked = walked.join(component)?;
            let _ = self.core.run(
                "create folder",
                self.core.session.create_dir(self.remote(&walked)),
            );
        }
        self.core
            .run("stat", self.core.session.metadata(self.remote(path)))
            .map(|_| ())
    }

    fn write_file(&self, path: &VfsPath, content: &mut dyn Read, cancel: &Cancel) -> VfsResult<()> {
        let parent = path.parent().unwrap_or_else(VfsPath::root);
        let name = path.name().unwrap_or("file");
        let staging = parent.join(&temporary_name(name, nonce()))?;
        let remote_staging = self.remote(&staging);
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
        let mut handle = self.core.run(
            "open for write",
            self.core
                .session
                .open_with_flags(remote_staging.clone(), flags),
        )?;

        let outcome = (|| -> VfsResult<()> {
            let mut buffer = vec![0u8; CHUNK];
            loop {
                cancel.check()?;
                let read = content.read(&mut buffer).map_err(crate::limits::uncarry)?;
                if read == 0 {
                    break;
                }
                let chunk = buffer.get(..read).unwrap_or_default().to_vec();
                self.core.run_io("write", handle.write_all(&chunk))?;
            }
            self.core.run_io("flush", handle.flush())
        })();
        let _ = self.core.run_io("close", handle.shutdown());

        if let Err(error) = outcome {
            // The file being replaced is untouched; only the staged name is
            // left behind, and it is removed here.
            let _ = self
                .core
                .run("delete", self.core.session.remove_file(remote_staging));
            return Err(error);
        }
        let final_remote = self.remote(path);
        let _ = self.core.run(
            "delete",
            self.core.session.remove_file(final_remote.clone()),
        );
        self.core.run(
            "rename",
            self.core.session.rename(remote_staging, final_remote),
        )
    }

    fn delete(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        if path.is_root() {
            return Err(VfsError::unsupported(
                "the root of a profile is not deleted",
            ));
        }
        let entry = self.metadata(path)?;
        if entry.is_dir() && !entry.is_link() {
            return self.delete_tree(path, cancel);
        }
        self.core
            .run("delete", self.core.session.remove_file(self.remote(path)))
    }

    fn rename(&self, from: &VfsPath, to: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        cancel.check()?;
        let _ = self
            .core
            .run("delete", self.core.session.remove_file(self.remote(to)));
        self.core.run(
            "rename",
            self.core.session.rename(self.remote(from), self.remote(to)),
        )
    }
}

/// Replace what no file system can hold in a server-supplied name.
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

/// Render a name safely for an error message.
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
    use tokio::io::AsyncWriteExt;

    #[test]
    fn a_hostile_name_is_kept_under_a_name_that_can_be_held() {
        for name in ["../escape", "a/b", "..", "", "x\u{7}y", "C:/x"] {
            let cleaned = sanitize(name);
            assert!(VfsPath::parse(&cleaned).is_ok(), "{cleaned:?}");
            assert!(child_path(&VfsPath::root(), &cleaned).is_ok());
        }
    }

    #[tokio::test]
    async fn an_oversized_sftp_packet_is_rejected_from_its_prefix() {
        let (client, mut server) = tokio::io::duplex(64);
        let sender = tokio::spawn(async move {
            server.write_all(&17u32.to_be_bytes()).await.unwrap();
        });
        let packet_limit_hit = Arc::new(AtomicBool::new(false));
        let mut stream = BoundedSftpStream::new(client, 16, packet_limit_hit.clone());
        let error = stream.read_u32().await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(packet_limit_hit.load(Ordering::Acquire));
        sender.await.unwrap();
    }

    #[tokio::test]
    async fn the_packet_limit_resets_after_a_complete_sftp_packet() {
        let (client, mut server) = tokio::io::duplex(64);
        let sender = tokio::spawn(async move {
            server.write_all(&4u32.to_be_bytes()).await.unwrap();
            server.write_all(b"test").await.unwrap();
            server.write_all(&17u32.to_be_bytes()).await.unwrap();
        });
        let packet_limit_hit = Arc::new(AtomicBool::new(false));
        let mut stream = BoundedSftpStream::new(client, 16, packet_limit_hit.clone());
        assert_eq!(stream.read_u32().await.unwrap(), 4);
        let mut body = [0u8; 4];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"test");
        assert_eq!(
            stream.read_u32().await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(packet_limit_hit.load(Ordering::Acquire));
        sender.await.unwrap();
    }

    #[tokio::test]
    async fn a_certificate_host_key_is_refused_even_when_its_key_is_recorded() {
        use russh::keys::ssh_key::certificate::{Builder, CertType};
        use russh::keys::{Algorithm, PrivateKey};

        let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let authority = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let mut builder =
            Builder::new(vec![7u8; 32], host_key.public_key().clone(), 0, u64::MAX).unwrap();
        builder.serial(1).unwrap();
        builder.cert_type(CertType::Host).unwrap();
        builder.valid_principal("127.0.0.1").unwrap();
        let certificate = builder.sign(&authority).unwrap();

        let directory = tempfile::tempdir().unwrap();
        let store = KnownHosts::new(directory.path().join("known_hosts"));
        let bytes = host_key.public_key().to_bytes().unwrap();
        let key_type = host_key.public_key().algorithm().to_string();
        store.record("127.0.0.1", 22, &key_type, &bytes).unwrap();

        let refusal = Arc::new(Mutex::new(None));
        let mut handler = SshHandler {
            known_hosts: Some(store),
            host: "127.0.0.1".to_owned(),
            port: 22,
            refusal: Arc::clone(&refusal),
        };
        let offered = PublicKeyOrCertificate::Certificate(certificate);
        let accepted = client::Handler::check_server_key(&mut handler, &offered)
            .await
            .unwrap();
        assert!(!accepted);
        let reason = refusal.lock().unwrap().take();
        assert!(
            matches!(reason, Some(VfsError::Unsupported { .. })),
            "{reason:?}"
        );

        let plain = PublicKeyOrCertificate::PublicKey {
            key: host_key.public_key().clone(),
            hash_alg: None,
        };
        assert!(client::Handler::check_server_key(&mut handler, &plain)
            .await
            .unwrap());
    }

    #[test]
    fn server_declared_resource_limits_keep_the_limit_error_kind() {
        let error = map_error(
            "list",
            &russh_sftp::client::error::Error::Limited(
                "directory listing exceeds the client resource limit".to_owned(),
            ),
        );

        assert!(matches!(error, VfsError::ResourceLimit { .. }));
    }

    #[test]
    fn oversized_packet_replaces_the_transport_timeout_with_a_limit_error() {
        let hit = AtomicBool::new(true);
        let result: VfsResult<()> =
            prioritize_packet_limit(&hit, Err(VfsError::timeout("list".to_owned())));

        assert!(matches!(result, Err(VfsError::ResourceLimit { .. })));
    }
}
