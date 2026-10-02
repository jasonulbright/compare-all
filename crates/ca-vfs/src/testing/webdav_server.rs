//! An HTTP share server over a folder, running in this process.
//!
//! It implements the methods the share client sends: the property query, the
//! two read methods, the write, the folder creation, the delete and the move.
//! Three extra modes let a test see what the client does with a server that
//! answers badly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::http_server::{range_start, under, HttpTestServer, Reply, Request};
use super::TestCertificate;

/// How the share server behaves.
#[derive(Debug, Clone, Default)]
pub struct Behaviour {
    /// Demand a password before answering.
    pub require_auth: bool,
    /// Answer every request with a redirect to this address.
    pub redirect_to: Option<String>,
    /// Wait this long before answering.
    pub delay: Option<Duration>,
}

/// A share server over `root`.
pub struct WebDavTestServer {
    inner: HttpTestServer,
}

impl WebDavTestServer {
    /// Start a plain server over `root`.
    pub fn start(root: &Path, behaviour: Behaviour) -> Self {
        Self {
            inner: HttpTestServer::start(handler(root.to_path_buf(), behaviour), None),
        }
    }

    /// Start a server over `root` that speaks transport security.
    pub fn start_secure(
        root: &Path,
        behaviour: Behaviour,
        certificate: Arc<TestCertificate>,
    ) -> Self {
        Self {
            inner: HttpTestServer::start(handler(root.to_path_buf(), behaviour), Some(certificate)),
        }
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.inner.port()
    }

    /// The address the share sits at.
    pub fn url(&self) -> String {
        self.inner.url()
    }
}

/// The share handler over `root`.
pub fn handler(root: PathBuf, behaviour: Behaviour) -> impl Fn(&Request) -> Reply + Send + Sync {
    move |request| {
        if let Some(delay) = behaviour.delay {
            std::thread::sleep(delay);
        }
        if let Some(target) = &behaviour.redirect_to {
            return Reply::status(302).with("Location", target);
        }
        if behaviour.require_auth && request.header("authorization").is_none() {
            return Reply::status(401).with("WWW-Authenticate", "Basic realm=\"share\"");
        }
        let Some(target) = under(&root, &request.path) else {
            return Reply::status(403);
        };
        match request.method.as_str() {
            "PROPFIND" => propfind(&root, &target, request),
            "GET" | "HEAD" => get(&target, request),
            "PUT" => {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&target, &request.body) {
                    Ok(()) => Reply::status(201),
                    Err(_) => Reply::status(403),
                }
            }
            "MKCOL" => match std::fs::create_dir(&target) {
                Ok(()) => Reply::status(201),
                Err(_) => Reply::status(405),
            },
            "DELETE" => {
                if !target.exists() {
                    return Reply::status(404);
                }
                let removed = if target.is_dir() {
                    std::fs::remove_dir_all(&target)
                } else {
                    std::fs::remove_file(&target)
                };
                match removed {
                    Ok(()) => Reply::status(204),
                    Err(_) => Reply::status(403),
                }
            }
            "MOVE" => {
                let Some(destination) = request.header("destination") else {
                    return Reply::status(400);
                };
                let path = destination
                    .split_once("://")
                    .map(|(_, rest)| rest.find('/').map_or("", |cut| &rest[cut..]))
                    .unwrap_or(destination);
                let Some(to) = under(&root, path) else {
                    return Reply::status(403);
                };
                if !target.exists() {
                    return Reply::status(404);
                }
                let _ = std::fs::remove_file(&to);
                match std::fs::rename(&target, &to) {
                    Ok(()) => Reply::status(201),
                    Err(_) => Reply::status(403),
                }
            }
            _ => Reply::status(405),
        }
    }
}

/// The property reply for one path.
fn propfind(root: &Path, target: &Path, request: &Request) -> Reply {
    if !target.exists() {
        return Reply::status(404);
    }
    let depth = request.header("depth").unwrap_or("1");
    let mut body = String::from(r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:">"#);
    body.push_str(&entry_xml(root, target));
    if depth != "0" && target.is_dir() {
        if let Ok(entries) = std::fs::read_dir(target) {
            for entry in entries.flatten() {
                body.push_str(&entry_xml(root, &entry.path()));
            }
        }
    }
    body.push_str("</D:multistatus>");
    Reply::body(207, "application/xml", body.into_bytes())
}

/// One property set.
fn entry_xml(root: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(root)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let href = format!("/{relative}");
    let Ok(metadata) = std::fs::metadata(path) else {
        return String::new();
    };
    let modified = crate::remote::timestamp::format_http_date(
        metadata
            .modified()
            .map_or(0, crate::remote::timestamp::unix_seconds),
    );
    if metadata.is_dir() {
        format!(
            "<D:response><D:href>{href}/</D:href><D:propstat><D:prop>\
             <D:resourcetype><D:collection/></D:resourcetype>\
             <D:getlastmodified>{modified}</D:getlastmodified>\
             </D:prop></D:propstat></D:response>"
        )
    } else {
        format!(
            "<D:response><D:href>{href}</D:href><D:propstat><D:prop><D:resourcetype/>\
             <D:getcontentlength>{}</D:getcontentlength>\
             <D:getlastmodified>{modified}</D:getlastmodified>\
             </D:prop></D:propstat></D:response>",
            metadata.len()
        )
    }
}

/// The content reply for one path.
fn get(target: &Path, request: &Request) -> Reply {
    let Ok(content) = std::fs::read(target) else {
        return Reply::status(404);
    };
    match range_start(request) {
        Some(start) => {
            let start = usize::try_from(start).unwrap_or(usize::MAX);
            if start > content.len() {
                return Reply::status(416);
            }
            let slice = content.get(start..).unwrap_or_default().to_vec();
            let end = content.len().saturating_sub(1);
            Reply::body(206, "application/octet-stream", slice).with(
                "Content-Range",
                &format!("bytes {start}-{end}/{}", content.len()),
            )
        }
        None => Reply::body(200, "application/octet-stream", content),
    }
}
