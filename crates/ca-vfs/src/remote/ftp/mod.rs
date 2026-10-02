//! The file transfer protocol, with and without transport security.
//!
//! One control connection is pooled and reused for listings and for the short
//! commands. Each read opens a connection of its own, because the handle the
//! caller gets back outlives the call that made it.
//!
//! Times in a plain listing are a local wall clock with no seconds, or a date
//! alone for an older file. The profile's server zone offset is applied and
//! the entry says how precise the result is, so the comparison layer allows a
//! tolerance rather than showing every file as differing by time. The
//! machine-readable listing states an instant in UTC and needs neither.

pub mod listing;
pub mod session;

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::ClientConfig;

use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{VfsError, VfsResult};
use crate::fs::{Capabilities, FileSystem, OpenFile};
use crate::limits::{Budget, LimitedReader};
use crate::path::VfsPath;

use crate::remote::profile::{FtpProfile, TransferType};
use crate::remote::secret::Secret;
use crate::remote::{child_path, nonce, temporary_name, RemoteContext};

use listing::{ListedEntry, ListedKind, ListedTime};
use session::{decode, Channel, Session};

/// Most bytes one listing may take. A listing is held whole and decoded, so
/// the file transfer ceiling would let a server claim gigabytes of memory.
const MAX_LISTING_BYTES: u64 = 64 * 1024 * 1024;

/// A file system backed by a file transfer server.
pub struct FtpFs {
    settings: FtpProfile,
    context: RemoteContext,
    password: Option<Secret>,
    tls: Option<Arc<ClientConfig>>,
    base: String,
    pooled: Mutex<Option<Session>>,
}

impl std::fmt::Debug for FtpFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpFs")
            .field("host", &self.settings.login.host)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl FtpFs {
    /// Log in and take the server's starting folder as the root.
    ///
    /// # Errors
    /// Returns whatever the connection or the login reports. No secret reaches
    /// any message.
    pub fn connect(
        settings: &FtpProfile,
        context: &RemoteContext,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        let password = context.secret(&settings.login.password);
        let tls = if settings.login.protocol.uses_tls() {
            Some(crate::remote::tls::client_config(&tls_options(settings)?)?)
        } else {
            None
        };
        let mut session = Session::open(
            settings,
            password.as_ref(),
            tls.clone(),
            context.deadline(),
            cancel,
        )?;
        let base = if settings.root_path.trim().is_empty() {
            let reply = session.expect("PWD")?;
            parse_pwd(&reply.text).unwrap_or_else(|| "/".to_owned())
        } else {
            let wanted = normalize_base(&settings.root_path);
            session.expect(&format!("CWD {wanted}"))?;
            wanted
        };
        Ok(Self {
            settings: settings.clone(),
            context: context.clone(),
            password,
            tls,
            base,
            pooled: Mutex::new(Some(session)),
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

    /// Open a connection of its own.
    fn fresh(&self, cancel: &Cancel) -> VfsResult<Session> {
        Session::open(
            &self.settings,
            self.password.as_ref(),
            self.tls.clone(),
            self.context.deadline(),
            cancel,
        )
    }

    /// Run `work` on the pooled connection, opening one when there is none.
    ///
    /// A connection that failed for a reason that leaves its state unknown is
    /// dropped rather than reused.
    fn with_session<T>(
        &self,
        cancel: &Cancel,
        work: impl FnOnce(&mut Session) -> VfsResult<T>,
    ) -> VfsResult<T> {
        cancel.check()?;
        let mut slot = self
            .pooled
            .lock()
            .map_err(|_| VfsError::network("the connection pool is poisoned"))?;
        let mut session = match slot.take() {
            Some(mut session) => {
                session.set_deadline(self.context.deadline());
                let idle =
                    Duration::from_secs(u64::from(self.settings.connection.keep_alive_seconds));
                if session.keep_alive(idle).is_err() {
                    self.fresh(cancel)?
                } else {
                    session
                }
            }
            None => self.fresh(cancel)?,
        };
        let result = work(&mut session);
        match &result {
            Ok(_) => *slot = Some(session),
            Err(error) if reusable(error) => *slot = Some(session),
            Err(_) => {}
        }
        result
    }

    /// The listing command for the current profile.
    fn list_command(&self, remote: &str) -> (bool, String) {
        let options = &self.settings.listing;
        if options.use_mlsd {
            return (true, format!("MLSD {remote}"));
        }
        let mut flags = String::new();
        if options.show_hidden {
            flags.push('a');
        }
        if options.force_long_format {
            flags.push('l');
        }
        if options.complete_timestamps {
            flags.push('T');
        }
        if options.resolve_links {
            flags.push('L');
        }
        if options.recursive {
            flags.push('R');
        }
        if flags.is_empty() {
            (false, format!("LIST {remote}"))
        } else {
            (false, format!("LIST -{flags} {remote}"))
        }
    }

    /// Read a whole data channel under the crate's ceilings.
    fn drain(&self, mut channel: Channel, cancel: &Cancel) -> VfsResult<Vec<u8>> {
        let mut limits = self.context.limits;
        limits.max_entry_bytes = limits.max_entry_bytes.min(MAX_LISTING_BYTES);
        limits.max_archive_bytes = limits.max_archive_bytes.min(MAX_LISTING_BYTES);
        let budget = Budget::new(limits.max_archive_bytes);
        crate::limits::read_bounded(&mut channel, 0, &limits, &budget, cancel)
    }

    /// The raw listing lines for `dir`.
    fn listing_lines(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<(bool, Vec<String>)> {
        let remote = self.remote(dir);
        let deadline = self.context.deadline();
        let range = self
            .settings
            .connection
            .active_port_range
            .as_ref()
            .map(|range| (range.first, range.last));
        let (wanted_machine, command) = self.list_command(&remote);
        let (machine, bytes) = self.with_session(cancel, |session| {
            let machine = wanted_machine && session.features.mlsd;
            let command = if machine {
                command.clone()
            } else if command.starts_with("MLSD") {
                format!("LIST {remote}")
            } else {
                command.clone()
            };
            let channel = session.open_data(&command, deadline, cancel, range)?;
            let bytes = self.drain(channel, cancel)?;
            // The transfer reply follows the data channel closing.
            let reply = session.read_reply()?;
            if !reply.is_positive() {
                return Err(VfsError::protocol(format!(
                    "the listing was refused: {}",
                    session::one_line(&reply.text)
                )));
            }
            Ok((machine, bytes))
        })?;
        let text = decode(&bytes, &self.settings.server.encoding);
        Ok((machine, text.lines().map(str::to_owned).collect()))
    }

    /// Parse the listing of `dir` into entries.
    fn parse_listing(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        let (machine, lines) = self.listing_lines(dir, cancel)?;
        let now = crate::remote::timestamp::unix_seconds(std::time::SystemTime::now());
        let mut listed: Vec<ListedEntry> = Vec::new();
        for line in lines {
            cancel.check()?;
            let parsed = if machine {
                listing::parse_machine_line(&line)
            } else {
                listing::parse_plain_line(&line, now)
            };
            if let Some(entry) = parsed {
                listed.push(entry);
            }
        }
        Ok(self.place(dir, listed))
    }

    /// Turn parsed lines into entries, keeping every one of them.
    ///
    /// A name a file system cannot hold is not dropped: it is placed under a
    /// name that can be held and carries the reason on the entry, because a
    /// listing showing fewer files than the server holds would be wrong about
    /// the server rather than merely limited.
    fn place(&self, dir: &VfsPath, listed: Vec<ListedEntry>) -> Vec<VfsEntry> {
        let mut out = Vec::with_capacity(listed.len());
        let mut taken = crate::remote::ListedNames::default();
        for item in listed {
            let mut reason: Option<String> = None;
            let cleaned = sanitize(&item.name);
            if cleaned != item.name {
                reason = Some(format!(
                    "the server listed a name that no file system can hold: {}",
                    escape(&item.name)
                ));
            }
            let (candidate, repeated) = taken.claim(cleaned);
            if repeated {
                reason.get_or_insert_with(|| {
                    format!(
                        "the server listed {} more than once, or twice with the same letters \
                         in a different case",
                        escape(&item.name)
                    )
                });
            }

            let Ok(path) = child_path(dir, &candidate) else {
                continue;
            };
            out.push(self.to_entry(path, candidate, &item, reason));
        }
        out
    }

    /// Build one entry from one parsed line.
    fn to_entry(
        &self,
        path: VfsPath,
        name: String,
        item: &ListedEntry,
        reason: Option<String>,
    ) -> VfsEntry {
        let link = match item.kind {
            ListedKind::Link => Some(if self.link_is_directory(&name) {
                VfsLinkKind::DirectoryLink
            } else {
                VfsLinkKind::FileLink
            }),
            _ => None,
        };
        let kind = match item.kind {
            ListedKind::Directory => EntryKind::Directory,
            ListedKind::File => EntryKind::File,
            ListedKind::Link => {
                if link == Some(VfsLinkKind::DirectoryLink) {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                }
            }
        };
        let (modified, fidelity) = match item.modified {
            Some(time) => {
                let (seconds, fidelity) =
                    apply_zone(time, self.settings.server.time_zone_offset_minutes);
                (
                    Some(crate::remote::timestamp::system_time(seconds)),
                    fidelity,
                )
            }
            None => (None, TimeFidelity::Utc),
        };
        let attributes = item.unix_mode.map(|mode| VfsAttributes {
            read_only: mode & 0o200 == 0,
            hidden: name.starts_with('.'),
            system: false,
            archive: false,
            windows_bits: None,
            unix_mode: Some(mode),
            uid: None,
            gid: None,
        });
        VfsEntry {
            path,
            name,
            kind,
            size: item.size.unwrap_or(0),
            size_is_exact: item.size.is_some() || kind == EntryKind::Directory,
            modified,
            time_fidelity: fidelity,
            created: None,
            attributes,
            crc32: None,
            link,
            version_info: None,
            refused: reason.is_some(),
            error: reason,
        }
    }

    /// Decide what a link points at.
    ///
    /// The rule needs no extra request: a name carrying an extension is a
    /// file and anything else is a folder. The setting that probes each link
    /// with a change-directory is not implemented, so a profile asking for it
    /// gets this rule.
    fn link_is_directory(&self, name: &str) -> bool {
        let _ = self.settings.listing.link_resolution;
        !name.contains('.')
    }

    /// Open a read at `offset`.
    ///
    /// The protocol restarts a stream at a byte offset, so a transfer that
    /// stopped part way continues rather than starting again.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] when the server does not restart a
    /// transfer, and whatever the exchange reports otherwise.
    pub fn open_at(&self, path: &VfsPath, offset: u64, cancel: &Cancel) -> VfsResult<OpenFile> {
        let remote = self.remote(path);
        let deadline = self.context.deadline();
        let range = self.active_range();
        let mut session = self.fresh(cancel)?;
        let _ = session.command(match self.settings.transfer.transfer_type {
            TransferType::Ascii => "TYPE A",
            TransferType::Binary | TransferType::Auto | TransferType::Unknown(_) => "TYPE I",
        });
        let size = size_of(&mut session, &remote);
        if offset > 0 {
            if !session.features.rest {
                return Err(VfsError::unsupported(
                    "the server does not restart a transfer at an offset",
                ));
            }
            session.expect(&format!("REST {offset}"))?;
        }
        let channel = session.open_data(&format!("RETR {remote}"), deadline, cancel, range)?;
        let length = size.map(|value| value.saturating_sub(offset));
        let budget = Budget::new(self.context.limits.max_archive_bytes);
        let reader = LimitedReader::new(
            Download {
                channel,
                _session: session,
            },
            0,
            self.context.limits,
            budget,
            cancel.clone(),
        );
        Ok(OpenFile::streaming(reader, length))
    }

    /// The port range an active transfer listens on.
    fn active_range(&self) -> Option<(u16, u16)> {
        self.settings
            .connection
            .active_port_range
            .as_ref()
            .map(|range| (range.first, range.last))
    }

    /// Set the modification time of a file the server holds.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] when the server has no such command.
    pub fn set_modified(&self, path: &VfsPath, seconds: i64, cancel: &Cancel) -> VfsResult<()> {
        let remote = self.remote(path);
        let stamp = crate::remote::timestamp::format_ftp_stamp(seconds);
        self.with_session(cancel, |session| {
            if !session.features.mfmt {
                return Err(VfsError::unsupported(
                    "the server does not set a modification time",
                ));
            }
            session.expect(&format!("MFMT {stamp} {remote}"))?;
            Ok(())
        })
    }

    /// Delete everything under `path`, then `path` itself.
    fn delete_tree(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let entries = self.parse_listing(path, cancel)?;
        for entry in entries {
            cancel.check()?;
            if entry.is_dir() && !entry.is_link() {
                self.delete_tree(&entry.path, cancel)?;
            } else {
                let remote = self.remote(&entry.path);
                self.with_session(cancel, |session| session.expect(&format!("DELE {remote}")))?;
            }
        }
        let remote = self.remote(path);
        self.with_session(cancel, |session| session.expect(&format!("RMD {remote}")))?;
        Ok(())
    }
}

/// The size of a file, where the server answers the query.
fn size_of(session: &mut Session, remote: &str) -> Option<u64> {
    if !session.features.size {
        return None;
    }
    let reply = session.command(&format!("SIZE {remote}")).ok()?;
    if !reply.is_positive() {
        return None;
    }
    reply.text.trim().parse().ok()
}

/// A read that owns the connection it came from.
struct Download {
    channel: Channel,
    _session: Session,
}

impl Read for Download {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.channel.read(buf)
    }
}

/// Rename `from` to `to` on one session.
fn rename_on(session: &mut Session, from: &str, to: &str) -> VfsResult<()> {
    session.expect(&format!("RNFR {from}"))?;
    session.expect(&format!("RNTO {to}"))?;
    Ok(())
}

/// True when a failed call leaves the connection usable.
fn reusable(error: &VfsError) -> bool {
    matches!(
        error,
        VfsError::NotFound { .. }
            | VfsError::NotADirectory { .. }
            | VfsError::IsADirectory { .. }
            | VfsError::AlreadyExists { .. }
            | VfsError::Unsupported { .. }
            | VfsError::InvalidPath(_)
            | VfsError::Protocol { .. }
    )
}

/// Apply the server's zone offset and say how precise the result is.
fn apply_zone(time: ListedTime, offset_minutes: i32) -> (i64, TimeFidelity) {
    let offset = i64::from(offset_minutes) * 60;
    match time {
        ListedTime::Utc(seconds) => (seconds, TimeFidelity::Utc),
        ListedTime::LocalMinute(seconds) => (
            seconds.saturating_sub(offset),
            TimeFidelity::MinutePrecision,
        ),
        ListedTime::LocalDay(seconds) => {
            (seconds.saturating_sub(offset), TimeFidelity::DayPrecision)
        }
    }
}

/// The folder named in a print-working-directory reply.
#[must_use]
pub fn parse_pwd(text: &str) -> Option<String> {
    let start = text.find('"')?;
    let rest = text.get(start + 1..)?;
    let end = rest.rfind('"')?;
    let value = rest.get(..end)?.replace("\"\"", "\"");
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Normalize the folder a profile opens at.
fn normalize_base(raw: &str) -> String {
    let trimmed = raw.trim().replace('\\', "/");
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

/// Replace what no file system can hold in a server-supplied name.
///
/// The result is one component. It is never empty, never `.` or `..`, and
/// carries no separator, no volume prefix, no NUL and no control character.
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

/// The transport security settings a file transfer profile states.
fn tls_options(settings: &FtpProfile) -> VfsResult<crate::remote::tls::TlsOptions> {
    Ok(crate::remote::tls::TlsOptions {
        min_version: settings.connection.min_tls_version.clone().try_into()?,
        max_version: settings.connection.max_tls_version.clone().try_into()?,
        pinned_fingerprints: settings.connection.pinned_certificate_fingerprints.clone(),
        accept_any_certificate: settings.connection.accept_any_certificate,
    })
}

impl FileSystem for FtpFs {
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
        let scheme = if self.settings.login.protocol.uses_tls() {
            "ftps"
        } else {
            "ftp"
        };
        format!("{scheme}://{}{}", self.settings.login.host, self.base)
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        self.parse_listing(dir, cancel)
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        if path.is_root() {
            return Ok(VfsEntry::directory(path.clone()));
        }
        let cancel = Cancel::new();
        let parent = path.parent().unwrap_or_else(VfsPath::root);
        let name = path.name().unwrap_or_default();
        let entries = self.parse_listing(&parent, &cancel)?;
        entries
            .into_iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })
    }

    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile> {
        self.open_at(path, 0, cancel)
    }

    fn create_dir(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let mut walked = VfsPath::root();
        for component in path.components() {
            walked = walked.join(component)?;
            let remote = self.remote(&walked);
            let _ = self.with_session(cancel, |session| session.command(&format!("MKD {remote}")));
        }
        let remote = self.remote(path);
        self.with_session(cancel, |session| {
            let reply = session.command(&format!("CWD {remote}"))?;
            if reply.is_positive() {
                Ok(())
            } else {
                Err(VfsError::protocol(format!(
                    "the folder was not created: {}",
                    session::one_line(&reply.text)
                )))
            }
        })
    }

    fn write_file(&self, path: &VfsPath, content: &mut dyn Read, cancel: &Cancel) -> VfsResult<()> {
        let final_remote = self.remote(path);
        let parent = path.parent().unwrap_or_else(VfsPath::root);
        let name = path.name().unwrap_or("file");
        let through_temporary = self.settings.transfer.upload_through_temporary_name;
        let temporary = temporary_name(name, nonce());
        let staging = if through_temporary {
            self.remote(&parent.join(&temporary)?)
        } else {
            final_remote.clone()
        };

        let deadline = self.context.deadline();
        let range = self.active_range();
        let outcome = self.with_session(cancel, |session| {
            let _ = session.command(match self.settings.transfer.transfer_type {
                TransferType::Ascii => "TYPE A",
                TransferType::Binary | TransferType::Auto | TransferType::Unknown(_) => "TYPE I",
            });
            let mut channel =
                session.open_data(&format!("STOR {staging}"), deadline, cancel, range)?;
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                cancel.check()?;
                let read = content.read(&mut buffer).map_err(crate::limits::uncarry)?;
                if read == 0 {
                    break;
                }
                channel
                    .write_all(buffer.get(..read).unwrap_or_default())
                    .map_err(crate::limits::uncarry)?;
            }
            channel.flush().map_err(crate::limits::uncarry)?;
            drop(channel);
            let reply = session.read_reply()?;
            if !reply.is_positive() {
                return Err(VfsError::protocol(format!(
                    "the upload was refused: {}",
                    session::one_line(&reply.text)
                )));
            }
            Ok(())
        });

        if outcome.is_err() {
            if through_temporary {
                // The old content is still in place; only the staged name is
                // left behind, and it is removed here.
                let _ = self.with_session(cancel, |session| {
                    session.command(&format!("DELE {staging}"))
                });
            }
            return outcome;
        }
        if !through_temporary {
            return Ok(());
        }
        let aside = self.remote(&parent.join(&temporary_name(name, nonce()))?);
        self.with_session(cancel, |session| {
            if rename_on(session, &staging, &final_remote).is_ok() {
                return Ok(());
            }
            // A server that does not rename over an existing name gets the old
            // file moved aside rather than deleted, so a refusal of the second
            // rename can put it back.
            let moved_aside = rename_on(session, &final_remote, &aside).is_ok();
            match rename_on(session, &staging, &final_remote) {
                Ok(()) => {
                    if moved_aside {
                        let _ = session.command(&format!("DELE {aside}"));
                    }
                    Ok(())
                }
                Err(error) => {
                    if moved_aside {
                        let _ = rename_on(session, &aside, &final_remote);
                    }
                    let _ = session.command(&format!("DELE {staging}"));
                    Err(error)
                }
            }
        })
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
        let remote = self.remote(path);
        self.with_session(cancel, |session| session.expect(&format!("DELE {remote}")))?;
        Ok(())
    }

    fn rename(&self, from: &VfsPath, to: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let source = self.remote(from);
        let target = self.remote(to);
        self.with_session(cancel, |session| {
            session.expect(&format!("RNFR {source}"))?;
            session.expect(&format!("RNTO {target}"))?;
            Ok(())
        })
    }
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
    fn a_zone_offset_at_the_ends_of_the_time_range_saturates() {
        assert_eq!(
            apply_zone(ListedTime::LocalMinute(i64::MIN), 60),
            (i64::MIN, TimeFidelity::MinutePrecision)
        );
        assert_eq!(
            apply_zone(ListedTime::LocalDay(i64::MAX), -60),
            (i64::MAX, TimeFidelity::DayPrecision)
        );
        assert_eq!(
            apply_zone(ListedTime::LocalMinute(i64::MIN + 1), i32::MAX),
            (i64::MIN, TimeFidelity::MinutePrecision)
        );
        assert_eq!(
            apply_zone(ListedTime::LocalMinute(7_200), 60),
            (3_600, TimeFidelity::MinutePrecision)
        );
    }

    #[test]
    fn a_working_directory_reply_parses() {
        assert_eq!(
            parse_pwd("\"/pub/data\" is current"),
            Some("/pub/data".to_owned())
        );
        assert_eq!(
            parse_pwd("\"/a\"\"b\" is current"),
            Some("/a\"b".to_owned())
        );
        assert_eq!(parse_pwd("no quotes"), None);
    }

    #[test]
    fn a_hostile_name_is_kept_under_a_name_that_can_be_held() {
        assert_eq!(sanitize("../escape"), ".._escape");
        assert_eq!(sanitize("a/b"), "a_b");
        assert_eq!(sanitize("C:/Windows"), "C__Windows");
        assert_eq!(sanitize(".."), "._");
        assert_eq!(sanitize(""), "unnamed");
        assert_eq!(sanitize("bad\u{7}name"), "bad_name");
        assert_eq!(sanitize("trailing."), "trailing_");
        for name in ["../escape", "a/b", "C:/x", "", "..", "x\0y"] {
            let cleaned = sanitize(name);
            assert!(VfsPath::parse(&cleaned).is_ok(), "{cleaned:?}");
        }
    }

    #[test]
    fn the_base_folder_is_normalized() {
        assert_eq!(normalize_base("  pub/data/ "), "/pub/data");
        assert_eq!(normalize_base("/"), "/");
        assert_eq!(normalize_base(""), "/");
    }

    #[test]
    fn a_wall_clock_time_carries_its_precision() {
        let (seconds, fidelity) = apply_zone(ListedTime::LocalMinute(3600), 60);
        assert_eq!(seconds, 0);
        assert_eq!(fidelity, TimeFidelity::MinutePrecision);
        let (seconds, fidelity) = apply_zone(ListedTime::Utc(3600), 60);
        assert_eq!(seconds, 3600);
        assert_eq!(fidelity, TimeFidelity::Utc);
        assert_eq!(
            apply_zone(ListedTime::LocalDay(0), 0).1,
            TimeFidelity::DayPrecision
        );
    }
}
