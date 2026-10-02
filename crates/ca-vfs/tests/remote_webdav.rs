//! The HTTP share protocol, against a server running in this process.

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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ca_vfs::remote::profile::{HttpAuthScheme, TlsSettings, WebDavProfile};
use ca_vfs::remote::webdav::WebDavFs;
use ca_vfs::remote::{MemorySecretStore, RemoteContext, SecretRef};
use ca_vfs::{Cancel, FileSystem, TimeFidelity, VfsError, VfsPath};

use ca_vfs::testing::http_server::{range_start, under, HttpTestServer, Reply, Request};
use ca_vfs::testing::TestCertificate;

/// How the share server behaves.
#[derive(Debug, Clone, Default)]
struct Behaviour {
    /// Demand a password before answering.
    require_auth: bool,
    /// Answer every request with a redirect to this address.
    redirect_to: Option<String>,
    /// Wait this long before answering.
    delay: Option<Duration>,
}

/// A folder with a small tree in it.
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

/// The share handler.
fn handler(root: PathBuf, behaviour: Behaviour) -> impl Fn(&Request) -> Reply + Send + Sync {
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
    let modified = ca_vfs::remote::timestamp::format_http_date(
        metadata
            .modified()
            .map_or(0, ca_vfs::remote::timestamp::unix_seconds),
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

/// A profile pointing at `server`.
fn profile(server: &HttpTestServer) -> WebDavProfile {
    WebDavProfile {
        url: server.url(),
        timeout_seconds: Some(10),
        // The server in this process speaks plain HTTP on the loopback
        // address, which is the one case the refusal has to be waived for.
        allow_plaintext_credentials: true,
        ..WebDavProfile::default()
    }
}

/// A context with one stored password.
fn context() -> RemoteContext {
    let store = Arc::new(MemorySecretStore::new());
    store.insert("password", "hunter2");
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(10);
    context
}

/// Start a plain server over a fixture and connect to it.
fn connected(behaviour: Behaviour) -> (tempfile::TempDir, HttpTestServer, WebDavFs) {
    let directory = fixture();
    let server = HttpTestServer::start(handler(directory.path().to_path_buf(), behaviour), None);
    let mut settings = profile(&server);
    settings.url = format!("http://127.0.0.1:{}", server.port());
    let fs = WebDavFs::connect(&settings, &context(), &Cancel::new()).expect("the server answers");
    (directory, server, fs)
}

#[test]
fn a_listing_reports_names_sizes_and_times() {
    let (_directory, _server, fs) = connected(Behaviour::default());
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    let report = entries
        .iter()
        .find(|entry| entry.name == "report.txt")
        .expect("the file is listed");
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
fn a_file_reads_back_and_a_ranged_read_resumes() {
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
fn a_write_reads_back_and_leaves_no_temporary_name() {
    let (directory, _server, fs) = connected(Behaviour::default());
    let path = VfsPath::parse("written.txt").unwrap();
    fs.write_file(&path, &mut b"written content".as_slice(), &Cancel::new())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join("written.txt")).unwrap(),
        "written content"
    );
    let names: Vec<String> = fs
        .list(&VfsPath::root(), &Cancel::new())
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert!(
        !names.iter().any(|name| name.contains("ca-upload")),
        "{names:?}"
    );
}

#[test]
fn an_interrupted_write_leaves_the_old_file_untouched() {
    let directory = fixture();
    let fail_move = Arc::new(AtomicBool::new(true));
    let root = directory.path().to_path_buf();
    let inner = handler(root, Behaviour::default());
    let flag = Arc::clone(&fail_move);
    let server = HttpTestServer::start(
        move |request: &Request| {
            if request.method == "MOVE" && flag.load(Ordering::SeqCst) {
                return Reply::status(403);
            }
            inner(request)
        },
        None,
    );
    let mut settings = profile(&server);
    settings.url = format!("http://127.0.0.1:{}", server.port());
    let fs = WebDavFs::connect(&settings, &context(), &Cancel::new()).unwrap();

    let path = VfsPath::parse("report.txt").unwrap();
    assert!(fs
        .write_file(&path, &mut b"replacement".as_slice(), &Cancel::new())
        .is_err());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("report.txt")).unwrap(),
        "hello remote world"
    );
    let left: Vec<String> = std::fs::read_dir(directory.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !left.iter().any(|name| name.contains("ca-upload")),
        "{left:?}"
    );
}

#[test]
fn rename_delete_and_create_folder_all_work() {
    let (directory, _server, fs) = connected(Behaviour::default());
    let cancel = Cancel::new();
    fs.create_dir(&VfsPath::parse("made/deeper").unwrap(), &cancel)
        .unwrap();
    assert!(directory.path().join("made").join("deeper").is_dir());

    fs.rename(
        &VfsPath::parse("report.txt").unwrap(),
        &VfsPath::parse("moved.txt").unwrap(),
        &cancel,
    )
    .unwrap();
    assert!(directory.path().join("moved.txt").is_file());

    fs.delete(&VfsPath::parse("moved.txt").unwrap(), &cancel)
        .unwrap();
    assert!(!directory.path().join("moved.txt").exists());
}

#[test]
fn a_hostile_listing_cannot_escape_the_root() {
    let directory = fixture();
    let server = HttpTestServer::start(
        |_request: &Request| {
            let mut body = String::from(r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:">"#);
            for href in [
                "/../../etc/passwd",
                "/share/..",
                "/share/with%07control.txt",
                "/share/Report.TXT",
                "/share/report.txt",
                "/share/ordinary.txt",
            ] {
                body.push_str(&format!(
                    "<D:response><D:href>{href}</D:href><D:propstat><D:prop>\
                     <D:resourcetype/><D:getcontentlength>10</D:getcontentlength>\
                     </D:prop></D:propstat></D:response>"
                ));
            }
            body.push_str("</D:multistatus>");
            Reply::body(207, "application/xml", body.into_bytes())
        },
        None,
    );
    let mut settings = profile(&server);
    settings.url = format!("http://127.0.0.1:{}", server.port());
    let fs = WebDavFs::connect(&settings, &context(), &Cancel::new()).unwrap();
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert!(!entries.is_empty());
    for entry in &entries {
        assert_eq!(entry.path.depth(), 1, "{:?} left the root", entry.path);
        assert_eq!(entry.refused, entry.error.is_some(), "{entry:?}");
        assert!(!entry.name.chars().any(char::is_control));
        assert!(!entry.name.contains('/'));
    }
    assert!(entries.iter().any(|entry| entry.name == "ordinary.txt"));
    let _ = directory;
}

#[test]
fn a_refused_account_is_a_typed_error_with_no_secret_in_it() {
    let directory = fixture();
    let behaviour = Behaviour {
        require_auth: true,
        ..Behaviour::default()
    };
    let server = HttpTestServer::start(
        move |request: &Request| {
            if request.header("authorization").is_some() {
                return Reply::status(403);
            }
            Reply::status(401).with("WWW-Authenticate", "Basic realm=\"share\"")
        },
        None,
    );
    let _ = behaviour;
    let mut settings = profile(&server);
    settings.url = format!("http://127.0.0.1:{}", server.port());
    settings.username = "operator".to_owned();
    settings.password = SecretRef::new("password");
    settings.auth = HttpAuthScheme::Basic;

    let Err(error) = WebDavFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("a refused account must not connect");
    };
    assert!(matches!(error, VfsError::AuthFailed { .. }), "{error}");
    assert!(!error.to_string().contains("hunter2"));
    let _ = directory;
}

#[test]
fn a_server_that_answers_too_slowly_times_out() {
    let directory = fixture();
    let behaviour = Behaviour {
        delay: Some(Duration::from_secs(3)),
        ..Behaviour::default()
    };
    let server = HttpTestServer::start(handler(directory.path().to_path_buf(), behaviour), None);
    let mut settings = profile(&server);
    settings.url = format!("http://127.0.0.1:{}", server.port());
    settings.timeout_seconds = Some(1);

    let Err(error) = WebDavFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("a slow server must not answer in time");
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
    let mut settings = profile(&server);
    settings.url = format!("http://127.0.0.1:{}", server.port());

    let cancel = Cancel::new();
    let raised = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        raised.cancel();
    });
    let outcome = WebDavFs::connect(&settings, &context(), &cancel);
    assert!(outcome.is_err());
}

#[test]
fn a_certificate_that_does_not_verify_is_refused_until_it_is_pinned() {
    let directory = fixture();
    let certificate = Arc::new(TestCertificate::new());
    let server = HttpTestServer::start(
        handler(directory.path().to_path_buf(), Behaviour::default()),
        Some(Arc::clone(&certificate)),
    );
    let mut settings = profile(&server);
    settings.url = format!("https://localhost:{}", server.port());

    let Err(error) = WebDavFs::connect(&settings, &context(), &Cancel::new()) else {
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
    let fs = WebDavFs::connect(&settings, &context(), &Cancel::new())
        .expect("the pinned certificate is accepted");
    assert!(fs
        .list(&VfsPath::root(), &Cancel::new())
        .unwrap()
        .iter()
        .any(|entry| entry.name == "report.txt"));
}

#[test]
fn a_secure_request_is_never_redirected_onto_a_plain_one() {
    let directory = fixture();
    let certificate = Arc::new(TestCertificate::new());
    let plain = HttpTestServer::start(
        handler(directory.path().to_path_buf(), Behaviour::default()),
        None,
    );
    let plain_url = format!("http://127.0.0.1:{}/", plain.port());
    let secure = HttpTestServer::start(
        handler(
            directory.path().to_path_buf(),
            Behaviour {
                redirect_to: Some(plain_url),
                ..Behaviour::default()
            },
        ),
        Some(Arc::clone(&certificate)),
    );

    let mut settings = profile(&secure);
    settings.url = format!("https://localhost:{}", secure.port());
    settings.tls = TlsSettings {
        pinned_certificate_fingerprints: vec![certificate.fingerprint()],
        ..TlsSettings::default()
    };
    let Err(error) = WebDavFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("a redirect onto a plain connection must be refused");
    };
    assert!(matches!(error, VfsError::Tls { .. }), "{error}");
}

#[test]
fn a_redirect_loop_ends_rather_than_running_forever() {
    let directory = fixture();
    let server = HttpTestServer::start(
        |_request: &Request| Reply::status(302).with("Location", "/again"),
        None,
    );
    let mut settings = profile(&server);
    settings.url = format!("http://127.0.0.1:{}", server.port());
    let Err(error) = WebDavFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("an endless redirect must not be followed");
    };
    assert!(error.to_string().contains("redirected"), "{error}");
    let _ = directory;
}
