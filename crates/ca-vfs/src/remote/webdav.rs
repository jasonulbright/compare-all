//! A share reached over HTTP.
//!
//! Listings come from a property request at depth one. Content is read with a
//! ranged get, so a transfer that stopped continues from where it was. An
//! upload is written under a temporary name and moved into place, so an
//! interrupted upload leaves the file it replaces untouched.
//!
//! The server states a time as an HTTP date, which names an instant in UTC to
//! the second, and a size as a byte count, so both are exact.

use std::io::Read;
use std::time::Duration;

use quick_xml::events::Event;

use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsEntry};
use crate::error::{VfsError, VfsResult};
use crate::fs::{Capabilities, FileSystem, OpenFile};
use crate::limits::{Budget, LimitedReader};
use crate::path::VfsPath;
use crate::remote::http::{
    decode_component, encode_component, Credentials, HttpClient, RequestBody, Response, Url,
};
use crate::remote::profile::{HttpAuthScheme, WebDavProfile};
use crate::remote::{child_path, nonce, temporary_name, RemoteContext};

/// The property request body. It names the four properties a listing needs,
/// so a server holding large custom properties does not send them.
const PROPFIND_BODY: &str = concat!(
    r#"<?xml version="1.0" encoding="utf-8"?>"#,
    r#"<D:propfind xmlns:D="DAV:"><D:prop>"#,
    r"<D:resourcetype/><D:getcontentlength/><D:getlastmodified/><D:creationdate/>",
    r"</D:prop></D:propfind>",
);

/// A file system backed by a share reached over HTTP.
#[derive(Debug)]
pub struct WebDavFs {
    client: HttpClient,
    base: Url,
    context: RemoteContext,
    recursive: bool,
}

impl WebDavFs {
    /// Open the share a profile names.
    ///
    /// # Errors
    /// Returns [`VfsError::Protocol`] when the address does not parse and
    /// whatever the first request reports.
    pub fn connect(
        settings: &WebDavProfile,
        context: &RemoteContext,
        cancel: &Cancel,
    ) -> VfsResult<Self> {
        let mut base = Url::parse(&settings.url)?;
        let trimmed = base.path.trim_end_matches('/').to_owned();
        base.path = trimmed;
        let credentials = if settings.username.is_empty() {
            None
        } else {
            Some(Credentials {
                username: settings.username.clone(),
                password: context
                    .secret(&settings.password)
                    .unwrap_or_else(|| crate::remote::Secret::new(String::new())),
                scheme: settings.auth.clone(),
                allow_plaintext: settings.allow_plaintext_credentials,
            })
        };
        let timeout = settings
            .timeout_seconds
            .map_or(context.call_timeout, |value| {
                Duration::from_secs(u64::from(value))
            });
        let tls = crate::remote::tls::TlsOptions::try_from(&settings.tls)?;
        let client = HttpClient::new(&tls, credentials, timeout)?;
        let fs = Self {
            client,
            base,
            context: context.clone(),
            recursive: settings.recursive_listings,
        };
        // A listing of the root proves the address, the account and the
        // certificate before a comparison starts.
        fs.propfind(&VfsPath::root(), "0", cancel)?;
        Ok(fs)
    }

    /// The address of one path in the share.
    fn url(&self, path: &VfsPath) -> Url {
        let mut target = self.base.path.clone();
        for component in path.components() {
            target.push('/');
            target.push_str(&encode_component(component));
        }
        if target.is_empty() {
            target.push('/');
        }
        self.base.with_path(&target)
    }

    /// Send a property request and read the reply.
    fn propfind(&self, path: &VfsPath, depth: &str, cancel: &Cancel) -> VfsResult<Vec<Property>> {
        let headers = vec![
            ("Depth".to_owned(), depth.to_owned()),
            ("Content-Type".to_owned(), "application/xml".to_owned()),
        ];
        let mut response = self.client.send(
            "PROPFIND",
            &self.url(path),
            &headers,
            RequestBody::Bytes(PROPFIND_BODY.as_bytes().to_vec()),
            cancel,
        )?;
        if response.status == 404 {
            return Err(VfsError::NotFound { path: path.clone() });
        }
        check(&response, "the listing")?;
        let body = response.read_body(&self.context.limits, cancel)?;
        parse_multistatus(&body)
    }

    /// Open a read at `offset`.
    ///
    /// # Errors
    /// Returns whatever the request reports.
    pub fn open_at(&self, path: &VfsPath, offset: u64, cancel: &Cancel) -> VfsResult<OpenFile> {
        let mut headers = Vec::new();
        if offset > 0 {
            headers.push(("Range".to_owned(), format!("bytes={offset}-")));
        }
        let response =
            self.client
                .send("GET", &self.url(path), &headers, RequestBody::Empty, cancel)?;
        match response.status {
            404 => return Err(VfsError::NotFound { path: path.clone() }),
            401 | 403 => {
                return Err(VfsError::auth_failed(format!(
                    "the server refused to read {path}"
                )))
            }
            416 => {
                return Err(VfsError::protocol(format!(
                    "the server does not hold {offset} bytes of {path}"
                )))
            }
            _ => {}
        }
        if offset > 0 && response.status != 206 {
            return Err(VfsError::unsupported(
                "the server does not restart a transfer at an offset",
            ));
        }
        check(&response, "the read")?;
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
}

/// One property set from a multi-status reply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Property {
    /// Address the properties describe.
    href: String,
    /// True when the entry holds other entries.
    collection: bool,
    /// Size where the server states one.
    length: Option<u64>,
    /// Modification time where the server states one.
    modified: Option<i64>,
    /// Creation time where the server states one.
    created: Option<i64>,
}

/// Read a multi-status reply.
fn parse_multistatus(body: &[u8]) -> VfsResult<Vec<Property>> {
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut out: Vec<Property> = Vec::new();
    let mut current = Property::default();
    let mut field: Option<Field> = None;
    let mut depth_in_response = false;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(start)) => match local_name(start.name().as_ref()) {
                b"response" => {
                    current = Property::default();
                    depth_in_response = true;
                }
                b"href" if depth_in_response && current.href.is_empty() => {
                    field = Some(Field::Href);
                }
                b"getcontentlength" => field = Some(Field::Length),
                b"getlastmodified" => field = Some(Field::Modified),
                b"creationdate" => field = Some(Field::Created),
                b"collection" => current.collection = true,
                _ => {}
            },
            Ok(Event::Empty(empty)) => {
                if local_name(empty.name().as_ref()) == b"collection" {
                    current.collection = true;
                }
            }
            Ok(Event::Text(text)) => {
                let value = String::from_utf8_lossy(text.as_ref()).into_owned();
                match field {
                    Some(Field::Href) => current.href = value,
                    Some(Field::Length) => current.length = value.trim().parse().ok(),
                    Some(Field::Modified) => {
                        current.modified = crate::remote::timestamp::parse_http_date(&value);
                    }
                    Some(Field::Created) => {
                        current.created = crate::remote::timestamp::parse_iso8601_utc(&value);
                    }
                    None => {}
                }
            }
            Ok(Event::End(end)) => {
                if local_name(end.name().as_ref()) == b"response" {
                    depth_in_response = false;
                    if !current.href.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
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
        if out.len() > 1_000_000 {
            return Err(VfsError::protocol(
                "the listing names more entries than one folder can hold".to_owned(),
            ));
        }
    }
    Ok(out)
}

/// Which property a text node belongs to.
#[derive(Debug, Clone, Copy)]
enum Field {
    Href,
    Length,
    Modified,
    Created,
}

/// The element name without its namespace prefix.
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|byte| *byte == b':') {
        Some(cut) => name.get(cut + 1..).unwrap_or(name),
        None => name,
    }
}

/// True when `href` names something on the same server as `base`.
///
/// A relative address always does. An absolute one must state the same scheme,
/// host and port; a reply that names another server describes nothing in this
/// share.
fn belongs_to(href: &str, base: &Url) -> bool {
    if !href.contains("://") {
        return true;
    }
    Url::parse(href).is_ok_and(|target| target.origin() == base.origin())
}

/// The last component of an address, decoded.
fn href_name(href: &str) -> Option<String> {
    let path = href.split_once("://").map_or(href, |(_, rest)| {
        rest.find('/')
            .map_or("", |cut| rest.get(cut..).unwrap_or(""))
    });
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let trimmed = path.trim_end_matches('/');
    let last = trimmed.rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    Some(decode_component(last))
}

/// Fail on a status that does not name success.
fn check(response: &Response, what: &str) -> VfsResult<()> {
    if response.is_success() || response.status == 207 {
        return Ok(());
    }
    match response.status {
        401 | 403 => Err(VfsError::auth_failed(format!(
            "{what} was refused with status {}",
            response.status
        ))),
        404 => Err(VfsError::protocol(format!("{what} found nothing"))),
        _ => Err(VfsError::protocol(format!(
            "{what} failed with status {}",
            response.status
        ))),
    }
}

impl FileSystem for WebDavFs {
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
        self.base.to_text()
    }

    fn list(&self, dir: &VfsPath, cancel: &Cancel) -> VfsResult<Vec<VfsEntry>> {
        // A request that covers a whole tree makes the server walk it in full
        // for every folder opened, and only the direct children are read from
        // the reply, so one level is asked for whatever the profile says.
        let _ = self.recursive;
        let found = self.propfind(dir, "1", cancel)?;
        let mut out = Vec::new();
        let mut taken = crate::remote::ListedNames::default();
        let self_name = dir.name().map(str::to_owned);
        for item in found {
            cancel.check()?;
            // An address naming another server describes nothing in this
            // share, so it is not placed in the listing.
            if !belongs_to(&item.href, &self.base) {
                continue;
            }
            let Some(raw) = href_name(&item.href) else {
                continue;
            };
            // The reply repeats the folder that was asked about.
            if Some(&raw) == self_name.as_ref() && item.collection {
                continue;
            }
            let mut reason = None;
            let mut name = sanitize(&raw);
            if name != raw {
                reason = Some(format!(
                    "the server listed a name that no file system can hold: {}",
                    escape(&raw)
                ));
            }
            let (claimed, repeated) = taken.claim(name);
            name = claimed;
            if repeated {
                reason.get_or_insert_with(|| {
                    format!(
                        "the server listed {} more than once, or twice with the same letters in \
                         a different case",
                        escape(&raw)
                    )
                });
            }
            let Ok(path) = child_path(dir, &name) else {
                continue;
            };
            out.push(VfsEntry {
                path,
                name,
                kind: if item.collection {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                size: item.length.unwrap_or(0),
                size_is_exact: item.length.is_some() || item.collection,
                modified: item.modified.map(crate::remote::timestamp::system_time),
                time_fidelity: TimeFidelity::Utc,
                created: item.created.map(crate::remote::timestamp::system_time),
                attributes: None,
                crc32: None,
                link: None,
                version_info: None,
                refused: reason.is_some(),
                error: reason,
            });
        }
        Ok(out)
    }

    fn metadata(&self, path: &VfsPath) -> VfsResult<VfsEntry> {
        let cancel = Cancel::new();
        let found = self.propfind(path, "0", &cancel)?;
        let item = found
            .first()
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
        let name = path.name().unwrap_or_default().to_owned();
        Ok(VfsEntry {
            path: path.clone(),
            name,
            kind: if item.collection {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            size: item.length.unwrap_or(0),
            size_is_exact: item.length.is_some() || item.collection,
            modified: item.modified.map(crate::remote::timestamp::system_time),
            time_fidelity: TimeFidelity::Utc,
            created: item.created.map(crate::remote::timestamp::system_time),
            attributes: None,
            crc32: None,
            link: None,
            version_info: None,
            error: None,
            refused: false,
        })
    }

    fn open(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<OpenFile> {
        self.open_at(path, 0, cancel)
    }

    fn create_dir(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let mut walked = VfsPath::root();
        for component in path.components() {
            walked = walked.join(component)?;
            let response =
                self.client
                    .send("MKCOL", &self.url(&walked), &[], RequestBody::Empty, cancel)?;
            // A folder that is already there is not a failure.
            if !response.is_success() && response.status != 405 {
                return Err(VfsError::protocol(format!(
                    "the folder {walked} was not created: status {}",
                    response.status
                )));
            }
        }
        Ok(())
    }

    fn write_file(&self, path: &VfsPath, content: &mut dyn Read, cancel: &Cancel) -> VfsResult<()> {
        let parent = path.parent().unwrap_or_else(VfsPath::root);
        let name = path.name().unwrap_or("file");
        let staging = parent.join(&temporary_name(name, nonce()))?;
        // The body length is not known in advance, so the content is taken in
        // full before the request starts. The ceiling bounds what that costs.
        let budget = Budget::new(self.context.limits.max_archive_bytes);
        let bytes = crate::limits::read_bounded(content, 0, &self.context.limits, &budget, cancel)?;
        let length = bytes.len() as u64;
        let response = self.client.send(
            "PUT",
            &self.url(&staging),
            &[("Content-Length".to_owned(), length.to_string())],
            RequestBody::Bytes(bytes),
            cancel,
        )?;
        if let Err(error) = check(&response, "the upload") {
            let _ = self.client.send(
                "DELETE",
                &self.url(&staging),
                &[],
                RequestBody::Empty,
                cancel,
            );
            return Err(error);
        }
        let moved = self.client.send(
            "MOVE",
            &self.url(&staging),
            &[
                ("Destination".to_owned(), self.url(path).to_text()),
                ("Overwrite".to_owned(), "T".to_owned()),
            ],
            RequestBody::Empty,
            cancel,
        )?;
        if let Err(error) = check(&moved, "the move into place") {
            let _ = self.client.send(
                "DELETE",
                &self.url(&staging),
                &[],
                RequestBody::Empty,
                cancel,
            );
            return Err(error);
        }
        Ok(())
    }

    fn delete(&self, path: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        if path.is_root() {
            return Err(VfsError::unsupported(
                "the root of a profile is not deleted",
            ));
        }
        let response =
            self.client
                .send("DELETE", &self.url(path), &[], RequestBody::Empty, cancel)?;
        if response.status == 404 {
            return Err(VfsError::NotFound { path: path.clone() });
        }
        check(&response, "the delete")
    }

    fn rename(&self, from: &VfsPath, to: &VfsPath, cancel: &Cancel) -> VfsResult<()> {
        let response = self.client.send(
            "MOVE",
            &self.url(from),
            &[
                ("Destination".to_owned(), self.url(to).to_text()),
                ("Overwrite".to_owned(), "T".to_owned()),
            ],
            RequestBody::Empty,
            cancel,
        )?;
        if response.status == 404 {
            return Err(VfsError::NotFound { path: from.clone() });
        }
        check(&response, "the move")
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

/// The authentication scheme a profile names, for the interface to show.
#[must_use]
pub fn scheme_label(scheme: &HttpAuthScheme) -> &'static str {
    match scheme {
        HttpAuthScheme::Negotiate => "answer the server",
        HttpAuthScheme::Basic => "basic",
        HttpAuthScheme::Digest => "digest",
        HttpAuthScheme::None => "none",
        HttpAuthScheme::Unknown(_) => "unknown",
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
    fn a_multi_status_reply_parses() {
        let body = br#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/share/</D:href>
    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat>
  </D:response>
  <D:response>
    <D:href>/share/report%20one.txt</D:href>
    <D:propstat><D:prop>
      <D:resourcetype/>
      <D:getcontentlength>1234</D:getcontentlength>
      <D:getlastmodified>Tue, 15 Nov 1994 12:45:26 GMT</D:getlastmodified>
    </D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;
        let found = parse_multistatus(body).unwrap();
        assert_eq!(found.len(), 2);
        let first = found.first().unwrap();
        assert!(first.collection);
        let second = found.get(1).unwrap();
        assert!(!second.collection);
        assert_eq!(second.length, Some(1234));
        assert_eq!(href_name(&second.href).as_deref(), Some("report one.txt"));
    }

    #[test]
    fn a_reply_that_is_not_xml_is_reported_rather_than_panicking() {
        assert!(parse_multistatus(b"<D:multistatus><<<").is_err());
        assert!(parse_multistatus(b"").unwrap().is_empty());
    }

    #[test]
    fn a_hostile_href_never_escapes_the_root() {
        assert_eq!(
            href_name("/share/../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(href_name("/share/..").as_deref(), Some(".."));
        assert_eq!(sanitize(".."), "._");
        assert_eq!(sanitize("a/b"), "a_b");
        assert!(child_path(&VfsPath::root(), &sanitize("..")).is_ok());
    }
}
