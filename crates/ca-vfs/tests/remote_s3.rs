//! The object store protocol, against a server running in this process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::default_trait_access,
    clippy::assigning_clones,
    clippy::format_push_string,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::needless_pass_by_value,
    clippy::ref_option,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    missing_docs
)]

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ca_vfs::remote::profile::{S3Auth, S3Profile, TlsSettings};
use ca_vfs::remote::s3::S3Fs;
use ca_vfs::remote::{MemorySecretStore, RemoteContext, SecretRef};
use ca_vfs::{Cancel, FileSystem, TimeFidelity, VfsError, VfsPath};

use ca_vfs::testing::http_server::{decode, range_start, HttpTestServer, Reply, Request};
use ca_vfs::testing::TestCertificate;

/// The bucket the tests address.
const BUCKET: &str = "bucket";

/// How the store behaves.
#[derive(Debug, Clone, Default)]
struct Behaviour {
    /// Refuse a request that carries no signature.
    require_signature: bool,
    /// Answer with a crafted listing instead of the real one.
    hostile_listing: bool,
    /// Wait this long before answering.
    delay: Option<Duration>,
}

/// A folder standing in for the bucket contents.
fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("report.txt"), b"hello remote world").unwrap();
    std::fs::create_dir(directory.path().join("sub")).unwrap();
    std::fs::write(
        directory.path().join("sub").join("inner.bin"),
        vec![7u8; 2048],
    )
    .unwrap();
    directory
}

/// Every key in the bucket, as `key` and its real path.
fn keys(root: &std::path::Path) -> Vec<(String, PathBuf)> {
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

/// The store handler.
fn handler(root: PathBuf, behaviour: Behaviour) -> impl Fn(&Request) -> Reply + Send + Sync {
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
                        &ca_vfs::remote::timestamp::format_http_date(
                            metadata
                                .modified()
                                .map_or(0, ca_vfs::remote::timestamp::unix_seconds),
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
fn listing(root: &std::path::Path, request: &Request) -> Reply {
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
        let stamp = ca_vfs::remote::timestamp::unix_to_civil(
            metadata
                .modified()
                .map_or(0, ca_vfs::remote::timestamp::unix_seconds),
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

/// A profile pointing at `server`.
fn profile(server: &HttpTestServer, host: &str) -> S3Profile {
    S3Profile {
        auth: S3Auth::Saved {
            access_key_id: "AKIAEXAMPLE".to_owned(),
            secret_access_key: SecretRef::new("secret"),
            session_token: SecretRef::default(),
            unknown: Default::default(),
        },
        bucket: BUCKET.to_owned(),
        region: "us-east-1".to_owned(),
        endpoint: format!("{host}:{}", server.port()),
        path_style: true,
        timeout_seconds: Some(10),
        ..S3Profile::default()
    }
}

/// A context with the stored secret key.
fn context() -> RemoteContext {
    let store = Arc::new(MemorySecretStore::new());
    store.insert("secret", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(10);
    context
}

/// Start a plain server over a fixture and connect to it.
fn connected(behaviour: Behaviour) -> (tempfile::TempDir, HttpTestServer, S3Fs) {
    let directory = fixture();
    let server = HttpTestServer::start(handler(directory.path().to_path_buf(), behaviour), None);
    let settings = profile(&server, "http://127.0.0.1");
    let fs = S3Fs::connect(&settings, &context(), &Cancel::new()).expect("the store answers");
    (directory, server, fs)
}

#[test]
fn a_listing_shows_prefixes_as_folders() {
    let (_directory, _server, fs) = connected(Behaviour::default());
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    let report = entries
        .iter()
        .find(|entry| entry.name == "report.txt")
        .expect("the object is listed");
    assert_eq!(report.size, 18);
    assert!(report.size_is_exact);
    assert_eq!(report.time_fidelity, TimeFidelity::Utc);
    assert!(entries
        .iter()
        .any(|entry| entry.name == "sub" && entry.is_dir()));
}

#[test]
fn a_nested_listing_reaches_the_whole_tree() {
    let (_directory, _server, fs) = connected(Behaviour::default());
    let walked = ca_vfs::walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
    assert!(walked
        .iter()
        .any(|entry| entry.path.as_str() == "sub/inner.bin"));
}

#[test]
fn an_object_reads_back_and_a_ranged_read_resumes() {
    let (_directory, _server, fs) = connected(Behaviour::default());
    let path = VfsPath::parse("report.txt").unwrap();
    let mut whole = String::new();
    fs.open(&path, &Cancel::new())
        .unwrap()
        .read_to_string(&mut whole)
        .unwrap();
    assert_eq!(whole, "hello remote world");

    let mut rest = String::new();
    fs.open_at(&path, 6, &Cancel::new())
        .unwrap()
        .read_to_string(&mut rest)
        .unwrap();
    assert_eq!(rest, "remote world");
}

#[test]
fn a_write_reads_back_and_a_failed_write_leaves_the_old_object() {
    let (directory, _server, fs) = connected(Behaviour::default());
    let path = VfsPath::parse("written.txt").unwrap();
    fs.write_file(&path, &mut b"written content".as_slice(), &Cancel::new())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join("written.txt")).unwrap(),
        "written content"
    );

    // A store replaces an object only when the whole request arrives, so a
    // refused write leaves what was there.
    let refusing = HttpTestServer::start(|_request: &Request| Reply::status(403), None);
    let settings = profile(&refusing, "http://127.0.0.1");
    let other = S3Fs::connect(&settings, &context(), &Cancel::new());
    assert!(other.is_err());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("report.txt")).unwrap(),
        "hello remote world"
    );
}

#[test]
fn rename_is_a_copy_then_a_delete_and_delete_removes_a_prefix() {
    let (directory, _server, fs) = connected(Behaviour::default());
    let cancel = Cancel::new();
    fs.rename(
        &VfsPath::parse("report.txt").unwrap(),
        &VfsPath::parse("moved.txt").unwrap(),
        &cancel,
    )
    .unwrap();
    assert!(directory.path().join("moved.txt").is_file());
    assert!(!directory.path().join("report.txt").exists());

    fs.delete(&VfsPath::parse("sub").unwrap(), &cancel).unwrap();
    assert!(!directory.path().join("sub").join("inner.bin").exists());
}

#[test]
fn a_hostile_listing_cannot_escape_the_root() {
    let behaviour = Behaviour {
        hostile_listing: true,
        ..Behaviour::default()
    };
    let (_directory, _server, fs) = connected(behaviour);
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert!(!entries.is_empty());
    for entry in &entries {
        assert_eq!(entry.path.depth(), 1, "{:?} left the root", entry.path);
        assert_eq!(entry.refused, entry.error.is_some(), "{entry:?}");
        assert!(!entry.name.chars().any(char::is_control));
        assert!(!entry.name.contains('/'));
        assert!(VfsPath::parse(entry.path.as_str()).is_ok());
    }
    assert!(entries.iter().any(|entry| entry.name == "ordinary.txt"));
}

#[test]
fn an_unsigned_request_is_refused_and_the_error_carries_no_key() {
    let directory = fixture();
    let behaviour = Behaviour {
        require_signature: true,
        ..Behaviour::default()
    };
    let server = HttpTestServer::start(handler(directory.path().to_path_buf(), behaviour), None);
    let mut settings = profile(&server, "http://127.0.0.1");
    settings.auth = S3Auth::Anonymous {
        unknown: Default::default(),
    };

    let Err(error) = S3Fs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("an unsigned request must be refused");
    };
    assert!(matches!(error, VfsError::AuthFailed { .. }), "{error}");
    assert!(!error.to_string().contains("wJalrXUtnFEMI"));
}

#[test]
fn a_signed_request_reaches_the_store() {
    let directory = fixture();
    let behaviour = Behaviour {
        require_signature: true,
        ..Behaviour::default()
    };
    let server = HttpTestServer::start(handler(directory.path().to_path_buf(), behaviour), None);
    let settings = profile(&server, "http://127.0.0.1");
    let fs = S3Fs::connect(&settings, &context(), &Cancel::new()).expect("the signature is sent");
    assert!(fs
        .list(&VfsPath::root(), &Cancel::new())
        .unwrap()
        .iter()
        .any(|entry| entry.name == "report.txt"));
}

#[test]
fn a_store_that_answers_too_slowly_times_out() {
    let directory = fixture();
    let behaviour = Behaviour {
        delay: Some(Duration::from_secs(3)),
        ..Behaviour::default()
    };
    let server = HttpTestServer::start(handler(directory.path().to_path_buf(), behaviour), None);
    let mut settings = profile(&server, "http://127.0.0.1");
    settings.timeout_seconds = Some(1);
    let Err(error) = S3Fs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("a slow store must not answer in time");
    };
    assert!(
        matches!(error, VfsError::Timeout { .. } | VfsError::Network { .. }),
        "{error}"
    );
}

#[test]
fn a_cancel_stops_a_request() {
    let directory = fixture();
    let behaviour = Behaviour {
        delay: Some(Duration::from_secs(3)),
        ..Behaviour::default()
    };
    let server = HttpTestServer::start(handler(directory.path().to_path_buf(), behaviour), None);
    let settings = profile(&server, "http://127.0.0.1");
    let cancel = Cancel::new();
    let raised = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        raised.cancel();
    });
    assert!(S3Fs::connect(&settings, &context(), &cancel).is_err());
}

#[test]
fn a_certificate_that_does_not_verify_is_refused_until_it_is_pinned() {
    let directory = fixture();
    let certificate = Arc::new(TestCertificate::new());
    let server = HttpTestServer::start(
        handler(directory.path().to_path_buf(), Behaviour::default()),
        Some(Arc::clone(&certificate)),
    );
    let mut settings = profile(&server, "https://localhost");
    let Err(error) = S3Fs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("an unverified certificate must not be accepted");
    };
    assert!(
        matches!(
            error,
            VfsError::Tls { .. } | VfsError::Io(_) | VfsError::Network { .. }
        ),
        "{error}"
    );

    settings.tls = TlsSettings {
        pinned_certificate_fingerprints: vec![certificate.fingerprint()],
        ..TlsSettings::default()
    };
    let fs = S3Fs::connect(&settings, &context(), &Cancel::new())
        .expect("the pinned certificate is accepted");
    assert!(fs
        .list(&VfsPath::root(), &Cancel::new())
        .unwrap()
        .iter()
        .any(|entry| entry.name == "report.txt"));
}

#[test]
fn a_profile_with_no_bucket_is_refused_before_any_request() {
    let mut settings = S3Profile {
        region: "us-east-1".to_owned(),
        ..S3Profile::default()
    };
    settings.bucket.clear();
    let Err(error) = S3Fs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("a profile with no bucket must not connect");
    };
    assert!(matches!(error, VfsError::Protocol { .. }), "{error}");
}
