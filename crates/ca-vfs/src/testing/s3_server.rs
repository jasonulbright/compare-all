//! An object store server over a folder, running in this process.
//!
//! It implements what the store client sends: the version two listing, the
//! object read with a range, the head query, the write, the server side copy
//! and the delete. Two extra modes let a test see what the client does with a
//! server that answers badly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::http_server::{decode, range_start, HttpTestServer, Reply, Request};
use super::TestCertificate;

/// The bucket name the server answers for.
pub const BUCKET: &str = "bucket";

/// How the store behaves.
#[derive(Debug, Clone, Default)]
pub struct Behaviour {
    /// Refuse a request that carries no signature.
    pub require_signature: bool,
    /// Answer with a crafted listing instead of the real one.
    pub hostile_listing: bool,
    /// Wait this long before answering.
    pub delay: Option<Duration>,
}

/// An object store over `root`.
pub struct S3TestServer {
    inner: HttpTestServer,
}

impl S3TestServer {
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

    /// The address the store sits at.
    pub fn url(&self) -> String {
        self.inner.url()
    }
}

/// Every key in the bucket, as `key` and its real path.
pub fn keys(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let key = path
                    .strip_prefix(root)
                    .map(|value| value.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                out.push((key, path));
            }
        }
    }
    out.sort();
    out
}

/// The store handler over `root`.
pub fn handler(root: PathBuf, behaviour: Behaviour) -> impl Fn(&Request) -> Reply + Send + Sync {
    move |request| {
        if let Some(delay) = behaviour.delay {
            std::thread::sleep(delay);
        }
        if behaviour.require_signature
            && !request
                .header("authorization")
                .is_some_and(|value| value.starts_with("AWS4-HMAC-SHA256"))
        {
            return Reply::status(403);
        }
        let prefix = format!("/{BUCKET}");
        let Some(rest) = request.path.strip_prefix(&prefix) else {
            return Reply::status(404);
        };
        let key = decode(rest.trim_start_matches('/'));

        if request.method == "GET" && request.parameter("list-type").as_deref() == Some("2") {
            if behaviour.hostile_listing {
                return hostile_listing();
            }
            return listing(&root, request);
        }
        let target = {
            let mut out = root.clone();
            for part in key.split('/') {
                if part.is_empty() || part == "." {
                    continue;
                }
                if part == ".." {
                    return Reply::status(403);
                }
                out.push(part);
            }
            out
        };

        match request.method.as_str() {
            "GET" => {
                let Ok(content) = std::fs::read(&target) else {
                    return Reply::status(404);
                };
                match range_start(request) {
                    Some(start) => {
                        let start = usize::try_from(start).unwrap_or(usize::MAX);
                        if start > content.len() {
                            return Reply::status(416);
                        }
                        Reply::body(
                            206,
                            "application/octet-stream",
                            content.get(start..).unwrap_or_default().to_vec(),
                        )
                    }
                    None => Reply::body(200, "application/octet-stream", content),
                }
            }
            "HEAD" => match std::fs::metadata(&target) {
                Ok(metadata) if metadata.is_file() => Reply::status(200)
                    .with("Content-Length", &metadata.len().to_string())
                    .with(
                        "Last-Modified",
                        &crate::remote::timestamp::format_http_date(
                            metadata
                                .modified()
                                .map_or(0, crate::remote::timestamp::unix_seconds),
                        ),
                    ),
                _ => Reply::status(404),
            },
            "PUT" => {
                if let Some(source) = request.header("x-amz-copy-source") {
                    let from = decode(source)
                        .trim_start_matches('/')
                        .trim_start_matches(BUCKET)
                        .trim_start_matches('/')
                        .to_owned();
                    let mut origin = root.clone();
                    for part in from.split('/') {
                        if part.is_empty() || part == ".." {
                            continue;
                        }
                        origin.push(part);
                    }
                    let Ok(content) = std::fs::read(&origin) else {
                        return Reply::status(404);
                    };
                    if let Some(parent) = target.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&target, content);
                    return Reply::body(200, "application/xml", b"<CopyObjectResult/>".to_vec());
                }
                if key.ends_with('/') {
                    let _ = std::fs::create_dir_all(&target);
                    return Reply::status(200);
                }
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&target, &request.body) {
                    Ok(()) => Reply::status(200),
                    Err(_) => Reply::status(403),
                }
            }
            "DELETE" => {
                let _ = std::fs::remove_file(&target);
                let _ = std::fs::remove_dir(&target);
                Reply::status(204)
            }
            _ => Reply::status(405),
        }
    }
}

/// The listing reply.
fn listing(root: &Path, request: &Request) -> Reply {
    let prefix = request.parameter("prefix").unwrap_or_default();
    let delimiter = request.parameter("delimiter").unwrap_or_default();
    let mut body = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult><Name>bucket</Name>"#,
    );
    let mut folders: Vec<String> = Vec::new();
    for (key, path) in keys(root) {
        if !key.starts_with(&prefix) {
            continue;
        }
        let rest = key.get(prefix.len()..).unwrap_or_default();
        if !delimiter.is_empty() {
            if let Some((head, _)) = rest.split_once('/') {
                let folder = format!("{prefix}{head}/");
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
                continue;
            }
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let stamp = crate::remote::timestamp::unix_to_civil(
            metadata
                .modified()
                .map_or(0, crate::remote::timestamp::unix_seconds),
        );
        body.push_str(&format!(
            "<Contents><Key>{key}</Key><Size>{}</Size>\
             <LastModified>{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z</LastModified>\
             <ETag>&quot;none&quot;</ETag></Contents>",
            metadata.len(),
            stamp.0,
            stamp.1,
            stamp.2,
            stamp.3,
            stamp.4,
            stamp.5
        ));
    }
    for folder in folders {
        body.push_str(&format!(
            "<CommonPrefixes><Prefix>{folder}</Prefix></CommonPrefixes>"
        ));
    }
    body.push_str("<IsTruncated>false</IsTruncated></ListBucketResult>");
    Reply::body(200, "application/xml", body.into_bytes())
}

/// A listing built to break a client that trusts it.
fn hostile_listing() -> Reply {
    let mut body = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult><Name>bucket</Name>"#,
    );
    for key in [
        "../escape.txt",
        "..",
        "with\u{7}control.txt",
        "Report.TXT",
        "report.txt",
        "ordinary.txt",
    ] {
        body.push_str(&format!(
            "<Contents><Key>{}</Key><Size>10</Size>\
             <LastModified>2024-01-02T03:04:05.000Z</LastModified></Contents>",
            key.replace('&', "&amp;").replace('<', "&lt;")
        ));
    }
    body.push_str("<IsTruncated>false</IsTruncated></ListBucketResult>");
    Reply::body(200, "application/xml", body.into_bytes())
}
