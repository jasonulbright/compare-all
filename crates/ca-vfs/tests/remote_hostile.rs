//! Servers that answer badly on purpose.
//!
//! Each test starts a server in this process on the loopback address with a
//! port the operating system picks, makes it answer the way a hostile server
//! would, and states what the client must do about it. Nothing reaches the
//! network.

#![cfg(all(feature = "ftp", feature = "webdav", feature = "s3"))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::format_push_string
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ca_vfs::remote::http::{Credentials, HttpClient, RequestBody, Url};
use ca_vfs::remote::profile::{
    FtpLogin, FtpProfile, FtpProtocol, HttpAuthScheme, S3Auth, S3Profile, Unknown, WebDavProfile,
};
use ca_vfs::remote::{KnownHosts, MemorySecretStore, RemoteContext, TlsOptions};
use ca_vfs::{Cancel, FileSystem, VfsError, VfsPath};

use ca_vfs::testing::http_server::{HttpTestServer, Reply, Request};

/// A context with a short timeout, so a test that must fail fails quickly.
fn context() -> RemoteContext {
    let store = Arc::new(MemorySecretStore::new());
    store.insert("password", "hunter2");
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(5);
    context
}

/// A client with no credentials and a short timeout.
fn plain_client() -> HttpClient {
    HttpClient::new(&TlsOptions::default(), None, Duration::from_secs(5)).unwrap()
}

// ------------------------------------------------------------------ raw server

/// A server that answers every connection with bytes a test chose.
struct RawServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Request bytes of every connection the server accepted.
    seen: Arc<Mutex<Vec<String>>>,
}

impl RawServer {
    /// Start a server that reads one request and answers with `reply`.
    fn start(reply: Vec<u8>) -> Self {
        Self::start_with(move |_| reply.clone())
    }

    /// Start a server that answers with whatever `answer` builds.
    fn start_with<F>(answer: F) -> Self
    where
        F: Fn(&str) -> Vec<u8> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(answer);

        let thread = {
            let stop = Arc::clone(&stop);
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let seen = Arc::clone(&seen);
                            let answer = Arc::clone(&answer);
                            std::thread::spawn(move || {
                                let _ = serve_raw(socket, &seen, answer.as_ref());
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
            stop,
            thread: Some(thread),
            seen,
        }
    }

    /// The address the server answers at.
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Every request the server read, as text.
    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read one request head, record it and write the chosen answer.
fn serve_raw(
    mut socket: TcpStream,
    seen: &Mutex<Vec<String>>,
    answer: &(dyn Fn(&str) -> Vec<u8> + Send + Sync),
) -> std::io::Result<()> {
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = String::new();
    let mut reader = BufReader::new(socket.try_clone()?);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        head.push_str(&line);
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    seen.lock().unwrap().push(head.clone());
    socket.write_all(&answer(&head))?;
    socket.flush()
}

// ------------------------------------------------------------------ http framing

#[test]
fn a_reply_that_frames_its_body_twice_is_refused() {
    let server = RawServer::start(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
            .to_vec(),
    );
    let url = Url::parse(&server.url()).unwrap();
    let error = plain_client()
        .send("GET", &url, &[], RequestBody::Empty, &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::Protocol { .. }), "{error}");
}

#[test]
fn a_reply_that_states_two_different_lengths_is_refused() {
    let server = RawServer::start(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 9\r\n\r\nhello".to_vec(),
    );
    let url = Url::parse(&server.url()).unwrap();
    let error = plain_client()
        .send("GET", &url, &[], RequestBody::Empty, &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::Protocol { .. }), "{error}");
}

#[test]
fn a_reply_that_names_an_encoding_this_client_does_not_decode_is_refused() {
    let server = RawServer::start(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n0\r\n\r\n".to_vec(),
    );
    let url = Url::parse(&server.url()).unwrap();
    let error = plain_client()
        .send("GET", &url, &[], RequestBody::Empty, &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::Protocol { .. }), "{error}");
}

#[test]
fn a_chunk_length_no_transfer_can_mean_is_refused() {
    let server = RawServer::start(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nx".to_vec(),
    );
    let url = Url::parse(&server.url()).unwrap();
    let mut response = plain_client()
        .send("GET", &url, &[], RequestBody::Empty, &Cancel::new())
        .unwrap();
    let mut buffer = Vec::new();
    assert!(response.body.read_to_end(&mut buffer).is_err());
}

#[test]
fn a_chunk_that_does_not_end_with_a_line_ending_is_refused() {
    let server = RawServer::start(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloGET /x HTTP/1.1\r\n\r\n"
            .to_vec(),
    );
    let url = Url::parse(&server.url()).unwrap();
    let mut response = plain_client()
        .send("GET", &url, &[], RequestBody::Empty, &Cancel::new())
        .unwrap();
    let mut buffer = Vec::new();
    assert!(response.body.read_to_end(&mut buffer).is_err());
}

#[test]
fn a_reply_that_never_ends_its_headers_is_refused() {
    let mut reply = b"HTTP/1.1 200 OK\r\n".to_vec();
    for index in 0..400 {
        reply.extend_from_slice(format!("X-Pad-{index}: 1\r\n").as_bytes());
    }
    let server = RawServer::start(reply);
    let url = Url::parse(&server.url()).unwrap();
    let error = plain_client()
        .send("GET", &url, &[], RequestBody::Empty, &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::Protocol { .. }), "{error}");
}

#[test]
fn a_body_shorter_than_the_declared_length_is_refused() {
    let server = RawServer::start(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec());
    let url = Url::parse(&server.url()).unwrap();
    let mut response = plain_client()
        .send("GET", &url, &[], RequestBody::Empty, &Cancel::new())
        .unwrap();
    let mut buffer = Vec::new();
    assert!(response.body.read_to_end(&mut buffer).is_err());
}

// ------------------------------------------------------------------ redirects

#[test]
fn a_redirect_to_another_server_carries_no_credential_and_no_signature() {
    let target = RawServer::start(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec());
    let target_url = target.url();
    let origin = RawServer::start_with(move |_| {
        format!("HTTP/1.1 302 Found\r\nLocation: {target_url}/moved\r\nContent-Length: 0\r\n\r\n")
            .into_bytes()
    });

    let credentials = Credentials {
        username: "operator".to_owned(),
        password: ca_vfs::remote::Secret::new("hunter2"),
        scheme: HttpAuthScheme::Basic,
        allow_plaintext: true,
    };
    let client = HttpClient::new(
        &TlsOptions::default(),
        Some(credentials),
        Duration::from_secs(5),
    )
    .unwrap();
    let url = Url::parse(&origin.url()).unwrap();
    let headers = vec![
        ("x-amz-date".to_owned(), "20240102T030405Z".to_owned()),
        (
            "Authorization".to_owned(),
            "AWS4-HMAC-SHA256 Credential=AKIA/x, Signature=deadbeef".to_owned(),
        ),
    ];
    let response = client
        .send("GET", &url, &headers, RequestBody::Empty, &Cancel::new())
        .unwrap();
    assert_eq!(response.status, 200);

    let first = target.requests().first().cloned().unwrap_or_default();
    let lowered = first.to_lowercase();
    assert!(!lowered.contains("authorization"), "{first}");
    assert!(!lowered.contains("x-amz-"), "{first}");
    assert!(!first.contains("deadbeef"), "{first}");
    assert!(!first.contains("hunter2"), "{first}");
    // The first server did see the signature, so the test proves the drop and
    // not merely that nothing was sent at all.
    let sent = origin.requests().first().cloned().unwrap_or_default();
    assert!(sent.to_lowercase().contains("x-amz-date"), "{sent}");
}

#[test]
fn a_redirect_to_another_server_never_carries_the_uploaded_content() {
    let target = RawServer::start(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n".to_vec());
    let target_url = target.url();
    let origin = RawServer::start_with(move |_| {
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {target_url}/moved\r\n\
             Content-Length: 0\r\n\r\n"
        )
        .into_bytes()
    });
    let url = Url::parse(&format!("{}/report.txt", origin.url())).unwrap();
    let outcome = plain_client().send(
        "PUT",
        &url,
        &[],
        RequestBody::Bytes(b"private content".to_vec()),
        &Cancel::new(),
    );
    assert!(
        outcome
            .as_ref()
            .map_or(true, |response| response.status != 201),
        "the upload was accepted by another server"
    );
    assert!(target.requests().is_empty(), "{:?}", target.requests());
}

#[test]
fn a_redirect_back_to_the_same_server_keeps_the_signature() {
    let server = RawServer::start_with(move |head| {
        if head.starts_with("GET /moved") {
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec()
        } else {
            b"HTTP/1.1 302 Found\r\nLocation: /moved\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    });
    let url = Url::parse(&server.url()).unwrap();
    let headers = vec![("x-amz-date".to_owned(), "20240102T030405Z".to_owned())];
    let response = plain_client()
        .send("GET", &url, &headers, RequestBody::Empty, &Cancel::new())
        .unwrap();
    assert_eq!(response.status, 200);
    let moved = server
        .requests()
        .into_iter()
        .find(|head| head.starts_with("GET /moved"))
        .unwrap_or_default();
    assert!(moved.to_lowercase().contains("x-amz-date"), "{moved}");
}

#[test]
fn an_address_with_a_line_ending_in_its_host_is_refused() {
    assert!(Url::parse("http://example.test\r\nX-Injected: 1/share").is_err());
    assert!(Url::parse("http:// /share").is_err());
    assert!(Url::parse("http:///share").is_err());
}

// ------------------------------------------------------------------ webdav

#[test]
fn a_password_is_not_sent_over_a_plain_connection_unless_the_profile_says_so() {
    let server = HttpTestServer::start(|_: &Request| Reply::status(401), None);
    let settings = WebDavProfile {
        url: server.url(),
        username: "operator".to_owned(),
        password: ca_vfs::remote::SecretRef::new("password"),
        auth: HttpAuthScheme::Basic,
        timeout_seconds: Some(5),
        ..WebDavProfile::default()
    };
    let error = ca_vfs::remote::webdav::WebDavFs::connect(&settings, &context(), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::Tls { .. }), "{error}");
    assert!(!error.to_string().contains("hunter2"));
}

#[test]
fn a_listing_request_never_asks_the_server_to_walk_the_whole_tree() {
    let depths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&depths);
    let server = HttpTestServer::start(
        move |request: &Request| {
            if request.method == "PROPFIND" {
                recorded
                    .lock()
                    .unwrap()
                    .push(request.header("depth").unwrap_or("").to_owned());
            }
            Reply::body(
                207,
                "application/xml",
                br#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/</D:href>
                    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype>
                    </D:prop></D:propstat></D:response></D:multistatus>"#
                    .to_vec(),
            )
        },
        None,
    );
    let settings = WebDavProfile {
        url: server.url(),
        recursive_listings: true,
        timeout_seconds: Some(5),
        ..WebDavProfile::default()
    };
    let fs =
        ca_vfs::remote::webdav::WebDavFs::connect(&settings, &context(), &Cancel::new()).unwrap();
    fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    let seen = depths.lock().unwrap().clone();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|value| value != "infinity"),
        "a request asked the server to walk the whole tree: {seen:?}"
    );
}

#[test]
fn an_address_naming_another_server_is_left_out_of_a_listing() {
    let server = HttpTestServer::start(
        |_: &Request| {
            Reply::body(
                207,
                "application/xml",
                br#"<D:multistatus xmlns:D="DAV:">
                    <D:response><D:href>/</D:href><D:propstat><D:prop>
                    <D:resourcetype><D:collection/></D:resourcetype>
                    </D:prop></D:propstat></D:response>
                    <D:response><D:href>http://elsewhere.invalid/secret.txt</D:href>
                    <D:propstat><D:prop><D:resourcetype/>
                    <D:getcontentlength>1</D:getcontentlength>
                    </D:prop></D:propstat></D:response>
                    <D:response><D:href>/mine.txt</D:href>
                    <D:propstat><D:prop><D:resourcetype/>
                    <D:getcontentlength>2</D:getcontentlength>
                    </D:prop></D:propstat></D:response>
                    </D:multistatus>"#
                    .to_vec(),
            )
        },
        None,
    );
    let settings = WebDavProfile {
        url: server.url(),
        timeout_seconds: Some(5),
        ..WebDavProfile::default()
    };
    let fs =
        ca_vfs::remote::webdav::WebDavFs::connect(&settings, &context(), &Cancel::new()).unwrap();
    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    let names: Vec<&str> = listed.iter().map(|entry| entry.name.as_str()).collect();
    assert!(names.contains(&"mine.txt"), "{names:?}");
    assert!(!names.contains(&"secret.txt"), "{names:?}");
}

#[test]
fn a_name_kept_apart_from_a_duplicate_never_lands_on_another_listed_name() {
    let server = HttpTestServer::start(
        |_: &Request| {
            let mut body = String::from(
                r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/</D:href><D:propstat>
                <D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop>
                </D:propstat></D:response>"#,
            );
            for name in ["a", "a~2", "A"] {
                body.push_str(&format!(
                    "<D:response><D:href>/{name}</D:href><D:propstat><D:prop>\
                     <D:resourcetype/><D:getcontentlength>1</D:getcontentlength>\
                     </D:prop></D:propstat></D:response>"
                ));
            }
            body.push_str("</D:multistatus>");
            Reply::body(207, "application/xml", body.into_bytes())
        },
        None,
    );
    let settings = WebDavProfile {
        url: server.url(),
        timeout_seconds: Some(5),
        ..WebDavProfile::default()
    };
    let fs =
        ca_vfs::remote::webdav::WebDavFs::connect(&settings, &context(), &Cancel::new()).unwrap();
    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(listed.len(), 3);
    let folded: std::collections::BTreeSet<String> = listed
        .iter()
        .map(|entry| entry.name.to_lowercase())
        .collect();
    assert_eq!(folded.len(), 3, "{listed:?}");
}

#[test]
fn a_modification_time_no_calendar_can_hold_does_not_end_the_listing() {
    let server = HttpTestServer::start(
        |_: &Request| {
            Reply::body(
                207,
                "application/xml",
                br#"<D:multistatus xmlns:D="DAV:">
                    <D:response><D:href>/huge.txt</D:href><D:propstat><D:prop>
                    <D:resourcetype/><D:getcontentlength>1</D:getcontentlength>
                    <D:getlastmodified>Tue, 99 Nov 9223372036854775807 99:99:99 GMT
                    </D:getlastmodified>
                    </D:prop></D:propstat></D:response></D:multistatus>"#
                    .to_vec(),
            )
        },
        None,
    );
    let settings = WebDavProfile {
        url: server.url(),
        timeout_seconds: Some(5),
        ..WebDavProfile::default()
    };
    let fs =
        ca_vfs::remote::webdav::WebDavFs::connect(&settings, &context(), &Cancel::new()).unwrap();
    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed.first().unwrap().name, "huge.txt");
}

// ------------------------------------------------------------------ object store

#[test]
fn a_copy_that_reports_a_failure_in_its_body_does_not_delete_the_source() {
    let deleted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&deleted);
    let server = HttpTestServer::start(
        move |request: &Request| match request.method.as_str() {
            "GET" => Reply::body(
                200,
                "application/xml",
                br"<ListBucketResult><Name>bucket</Name><IsTruncated>false</IsTruncated>
                    <Contents><Key>report.txt</Key><Size>3</Size>
                    <LastModified>2024-01-02T03:04:05.000Z</LastModified></Contents>
                    </ListBucketResult>"
                    .to_vec(),
            ),
            // The store answers a copy with a success status and then states
            // the failure in the body.
            "PUT" => Reply::body(
                200,
                "application/xml",
                b"<Error><Code>InternalError</Code></Error>".to_vec(),
            ),
            "DELETE" => {
                recorded.lock().unwrap().push(request.decoded_path());
                Reply::status(204)
            }
            _ => Reply::status(404),
        },
        None,
    );
    let settings = S3Profile {
        auth: S3Auth::Anonymous {
            unknown: Unknown::default(),
        },
        bucket: "bucket".to_owned(),
        endpoint: server.url(),
        path_style: true,
        timeout_seconds: Some(5),
        ..S3Profile::default()
    };
    let fs = ca_vfs::remote::s3::S3Fs::connect(&settings, &context(), &Cancel::new()).unwrap();
    let error = fs
        .rename(
            &VfsPath::parse("report.txt").unwrap(),
            &VfsPath::parse("moved.txt").unwrap(),
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(matches!(error, VfsError::Protocol { .. }), "{error}");
    assert!(error.to_string().contains("InternalError"), "{error}");
    assert!(
        deleted.lock().unwrap().is_empty(),
        "the source was deleted after a copy that did not complete"
    );
}

#[test]
fn a_clock_that_disagrees_with_the_store_is_reported_as_itself() {
    let server = HttpTestServer::start(
        |_: &Request| {
            Reply::body(
                403,
                "application/xml",
                b"<Error><Code>RequestTimeTooSkewed</Code></Error>".to_vec(),
            )
        },
        None,
    );
    let settings = S3Profile {
        auth: S3Auth::Anonymous {
            unknown: Unknown::default(),
        },
        bucket: "bucket".to_owned(),
        endpoint: server.url(),
        path_style: true,
        timeout_seconds: Some(5),
        ..S3Profile::default()
    };
    let error =
        ca_vfs::remote::s3::S3Fs::connect(&settings, &context(), &Cancel::new()).unwrap_err();
    assert!(error.to_string().contains("clock"), "{error}");
}

#[test]
fn a_continuation_token_the_store_repeats_ends_the_listing() {
    let server = HttpTestServer::start(
        |_: &Request| {
            Reply::body(
                200,
                "application/xml",
                br"<ListBucketResult><Name>bucket</Name><IsTruncated>true</IsTruncated>
                    <Contents><Key>one.txt</Key><Size>1</Size></Contents>
                    <NextContinuationToken>same</NextContinuationToken>
                    </ListBucketResult>"
                    .to_vec(),
            )
        },
        None,
    );
    let settings = S3Profile {
        auth: S3Auth::Anonymous {
            unknown: Unknown::default(),
        },
        bucket: "bucket".to_owned(),
        endpoint: server.url(),
        path_style: true,
        timeout_seconds: Some(5),
        ..S3Profile::default()
    };
    let fs = ca_vfs::remote::s3::S3Fs::connect(&settings, &context(), &Cancel::new()).unwrap();
    // The second page repeats the token of the first, so the walk stops rather
    // than asking for the same page until the page ceiling.
    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert!(!listed.is_empty());
}

/// How many header lines of `head` carry `name`, in any letter case.
fn header_count(head: &str, name: &str) -> usize {
    head.lines()
        .filter(|line| {
            line.split_once(':')
                .is_some_and(|(field, _)| field.trim().eq_ignore_ascii_case(name))
        })
        .count()
}

#[test]
fn a_caller_framing_header_never_gives_a_request_two_of_them() {
    let server = RawServer::start(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec());
    let url = Url::parse(&format!("{}/object", server.url())).unwrap();
    let headers = vec![
        ("Content-Length".to_owned(), "99".to_owned()),
        ("transfer-encoding".to_owned(), "chunked".to_owned()),
    ];
    let response = plain_client()
        .send(
            "PUT",
            &url,
            &headers,
            RequestBody::Bytes(b"abc".to_vec()),
            &Cancel::new(),
        )
        .unwrap();
    assert_eq!(response.status, 200);
    let head = server.requests().first().cloned().unwrap_or_default();
    assert_eq!(header_count(&head, "content-length"), 1, "{head}");
    assert_eq!(header_count(&head, "transfer-encoding"), 0, "{head}");
    assert!(
        head.to_ascii_lowercase().contains("content-length: 3"),
        "{head}"
    );
}

#[test]
fn an_object_store_upload_carries_one_content_length() {
    let server = RawServer::start(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec());
    let settings = S3Profile {
        auth: S3Auth::Anonymous {
            unknown: Unknown::default(),
        },
        bucket: "bucket".to_owned(),
        endpoint: server.url(),
        path_style: true,
        timeout_seconds: Some(5),
        ..S3Profile::default()
    };
    let fs = ca_vfs::remote::s3::S3Fs::connect(&settings, &context(), &Cancel::new()).unwrap();
    let mut content: &[u8] = b"hello";
    fs.write_file(
        &VfsPath::parse("report.txt").unwrap(),
        &mut content,
        &Cancel::new(),
    )
    .unwrap();
    fs.create_dir(&VfsPath::parse("folder").unwrap(), &Cancel::new())
        .unwrap();
    let puts: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|head| head.starts_with("PUT "))
        .collect();
    assert_eq!(puts.len(), 2, "{puts:?}");
    for head in &puts {
        assert!(header_count(head, "content-length") <= 1, "{head}");
    }
    let upload = puts
        .iter()
        .find(|head| head.contains("report.txt"))
        .unwrap();
    assert_eq!(header_count(upload, "content-length"), 1, "{upload}");
}

// ------------------------------------------------------------------ file transfer

/// A file transfer server that names a third party in its passive reply while
/// serving the data on its own address.
struct BouncingFtpServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The address the passive reply names, which nothing listens on.
    announced: String,
}

impl BouncingFtpServer {
    fn start(announced: &str) -> Self {
        Self::start_repeating(announced, 1)
    }

    /// Start a server whose listing repeats one line `repeat` times.
    fn start_repeating(announced: &str, repeat: usize) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            let announced = announced.to_owned();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let announced = announced.clone();
                            std::thread::spawn(move || {
                                let _ = serve_bouncing(socket, &announced, repeat);
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
            stop,
            thread: Some(thread),
            announced: announced.to_owned(),
        }
    }
}

impl Drop for BouncingFtpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answer one control connection, naming `announced` as the data address.
fn serve_bouncing(socket: TcpStream, announced: &str, repeat: usize) -> std::io::Result<()> {
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = socket.try_clone()?;
    let mut reader = BufReader::new(socket);
    writer.write_all(b"220 ready\r\n")?;
    let mut data: Option<TcpListener> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let command = line.trim_end().to_ascii_uppercase();
        if command.starts_with("USER") || command.starts_with("PASS") {
            writer.write_all(b"230 in\r\n")?;
        } else if command.starts_with("FEAT") {
            writer.write_all(b"211-features\r\n SIZE\r\n211 end\r\n")?;
        } else if command.starts_with("PASV") {
            // The data socket listens on the loopback address; the reply names
            // a different one.
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            let port = listener.local_addr()?.port();
            let octets: Vec<&str> = announced.split('.').collect();
            writer.write_all(
                format!(
                    "227 Entering Passive Mode ({},{},{},{},{},{})\r\n",
                    octets.first().copied().unwrap_or("0"),
                    octets.get(1).copied().unwrap_or("0"),
                    octets.get(2).copied().unwrap_or("0"),
                    octets.get(3).copied().unwrap_or("0"),
                    port / 256,
                    port % 256
                )
                .as_bytes(),
            )?;
            data = Some(listener);
        } else if command.starts_with("LIST") {
            writer.write_all(b"150 opening\r\n")?;
            if let Some(listener) = data.take() {
                if let Ok((mut peer, _)) = listener.accept() {
                    let line = b"-rw-r--r--  1 o g  3 Jan  2 03:04 report.txt\r\n";
                    for _ in 0..repeat {
                        if peer.write_all(line).is_err() {
                            break;
                        }
                    }
                }
            }
            writer.write_all(b"226 done\r\n")?;
        } else if command.starts_with("QUIT") {
            writer.write_all(b"221 bye\r\n")?;
            return Ok(());
        } else {
            writer.write_all(b"200 ok\r\n")?;
        }
    }
}

#[test]
fn a_passive_reply_naming_a_third_party_does_not_move_the_transfer_there() {
    // The address is reserved for documentation, so nothing on the machine or
    // the network answers on it. With the reply believed, the listing would
    // stall against it and then fail.
    let server = BouncingFtpServer::start("203.0.113.9");
    let settings = FtpProfile {
        login: FtpLogin {
            host: "127.0.0.1".to_owned(),
            port: Some(server.port),
            protocol: FtpProtocol::Ftp,
            anonymous: true,
            ..FtpLogin::default()
        },
        ..FtpProfile::default()
    };
    assert_eq!(server.announced, "203.0.113.9");

    let fs = ca_vfs::remote::ftp::FtpFs::connect(&settings, &context(), &Cancel::new()).unwrap();
    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed.first().unwrap().name, "report.txt");
}

#[test]
fn a_listing_larger_than_the_metadata_ceiling_is_refused() {
    // Forty-eight bytes a line; the lines add up to more than the ceiling a
    // listing may take, while staying far below the file transfer ceiling.
    let server = BouncingFtpServer::start_repeating("127.0.0.1", 1_500_000);
    let settings = FtpProfile {
        login: FtpLogin {
            host: "127.0.0.1".to_owned(),
            port: Some(server.port),
            protocol: FtpProtocol::Ftp,
            anonymous: true,
            ..FtpLogin::default()
        },
        ..FtpProfile::default()
    };
    let mut context = context();
    context.call_timeout = Duration::from_secs(30);
    let fs = ca_vfs::remote::ftp::FtpFs::connect(&settings, &context, &Cancel::new()).unwrap();
    let outcome = fs.list(&VfsPath::root(), &Cancel::new());
    let Err(error) = outcome else {
        panic!(
            "a listing of {} entries was accepted",
            outcome.map_or(0, |listed| listed.len())
        );
    };
    assert!(
        matches!(
            error,
            VfsError::LimitExceeded { .. } | VfsError::ResourceLimit { .. }
        ),
        "{error}"
    );
}

/// What an active-mode server does once the client asks for a listing.
#[derive(Clone, Copy)]
enum ActiveServer {
    /// Acknowledge the listing and never open the data connection.
    NeverConnects,
    /// Try the announced port on another loopback address first, then serve
    /// the listing from the control connection's address.
    ProbesThenServes,
}

/// Serve one active-mode control connection. `probe` receives whether the
/// announced port answered on another loopback address.
fn serve_active(
    socket: TcpStream,
    behaviour: ActiveServer,
    probe: &Mutex<Option<bool>>,
) -> std::io::Result<()> {
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = socket.try_clone()?;
    let mut reader = BufReader::new(socket);
    writer.write_all(b"220 ready\r\n")?;
    let mut data_port: Option<u16> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let command = line.trim_end().to_ascii_uppercase();
        if command.starts_with("USER") || command.starts_with("PASS") {
            writer.write_all(b"230 in\r\n")?;
        } else if command.starts_with("FEAT") {
            writer.write_all(b"211-features\r\n SIZE\r\n211 end\r\n")?;
        } else if let Some(argument) = command.strip_prefix("PORT ") {
            let numbers: Vec<u16> = argument
                .split(',')
                .filter_map(|part| part.trim().parse().ok())
                .collect();
            data_port = match (numbers.get(4), numbers.get(5)) {
                (Some(high), Some(low)) => Some(high * 256 + low),
                _ => None,
            };
            writer.write_all(b"200 ok\r\n")?;
        } else if command.starts_with("LIST") {
            writer.write_all(b"150 opening\r\n")?;
            let Some(port) = data_port.take() else {
                continue;
            };
            if let ActiveServer::ProbesThenServes = behaviour {
                let other = std::net::SocketAddr::from(([127, 0, 0, 2], port));
                let reached = TcpStream::connect_timeout(&other, Duration::from_secs(1)).is_ok();
                *probe.lock().unwrap() = Some(reached);
                let mut peer = TcpStream::connect(("127.0.0.1", port))?;
                peer.write_all(b"-rw-r--r--  1 o g  3 Jan  2 03:04 report.txt\r\n")?;
                drop(peer);
                writer.write_all(b"226 done\r\n")?;
            }
        } else if command.starts_with("QUIT") {
            writer.write_all(b"221 bye\r\n")?;
            return Ok(());
        } else {
            writer.write_all(b"200 ok\r\n")?;
        }
    }
}

/// Start an active-mode server on the loopback address and return its port,
/// the probe result slot and a stop flag.
fn start_active(behaviour: ActiveServer) -> (u16, Arc<Mutex<Option<bool>>>, Arc<AtomicBool>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let probe = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let probe = Arc::clone(&probe);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let probe = Arc::clone(&probe);
                        std::thread::spawn(move || {
                            let _ = serve_active(socket, behaviour, &probe);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
    }
    (port, probe, stop)
}

/// An anonymous active-mode profile for a server on `port`.
fn active_profile(port: u16) -> FtpProfile {
    let mut settings = FtpProfile {
        login: FtpLogin {
            host: "127.0.0.1".to_owned(),
            port: Some(port),
            protocol: FtpProtocol::Ftp,
            anonymous: true,
            ..FtpLogin::default()
        },
        ..FtpProfile::default()
    };
    settings.connection.passive = false;
    settings
}

/// Run a listing on another thread and give up on it after a bound, so a
/// client that waits without end fails the test rather than hanging it.
fn bounded_listing(
    settings: FtpProfile,
    context: RemoteContext,
    cancel: Cancel,
) -> Option<Result<usize, VfsError>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = ca_vfs::remote::ftp::FtpFs::connect(&settings, &context, &cancel)
            .and_then(|fs| fs.list(&VfsPath::root(), &cancel))
            .map(|listed| listed.len());
        let _ = sender.send(outcome);
    });
    receiver.recv_timeout(Duration::from_secs(30)).ok()
}

#[test]
fn an_active_transfer_the_server_never_opens_ends_at_the_deadline() {
    let (port, _probe, stop) = start_active(ActiveServer::NeverConnects);
    let mut context = context();
    context.call_timeout = Duration::from_secs(1);
    let outcome = bounded_listing(active_profile(port), context, Cancel::new());
    stop.store(true, Ordering::SeqCst);
    let Some(outcome) = outcome else {
        panic!("the client still waits for a data connection past its deadline");
    };
    assert!(outcome.is_err(), "{outcome:?}");
}

#[test]
fn an_active_transfer_the_server_never_opens_ends_on_cancel() {
    let (port, _probe, stop) = start_active(ActiveServer::NeverConnects);
    let mut context = context();
    context.call_timeout = Duration::from_secs(120);
    let cancel = Cancel::new();
    let raised = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        raised.cancel();
    });
    let outcome = bounded_listing(active_profile(port), context, cancel);
    stop.store(true, Ordering::SeqCst);
    let Some(outcome) = outcome else {
        panic!("the client still waits for a data connection after a cancel");
    };
    assert!(outcome.is_err(), "{outcome:?}");
}

#[test]
fn an_active_transfer_listens_only_on_the_control_connection_address() {
    // Probing another loopback address shows whether the listening socket
    // was bound to every interface. Hosts that route only one loopback
    // address refuse the probe either way.
    let (port, probe, stop) = start_active(ActiveServer::ProbesThenServes);
    let outcome = bounded_listing(active_profile(port), context(), Cancel::new());
    stop.store(true, Ordering::SeqCst);
    let Some(outcome) = outcome else {
        panic!("the active listing did not finish");
    };
    assert_eq!(outcome.unwrap(), 1);
    assert_eq!(*probe.lock().unwrap(), Some(false));
}

// ------------------------------------------------------------------ host keys

#[test]
fn a_recorded_key_survives_a_store_whose_last_line_has_no_line_ending() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("known_hosts");
    std::fs::write(&path, "first.test ssh-ed25519 AAAAfirst").unwrap();
    let store = KnownHosts::new(&path);
    store
        .record("second.test", 22, "ssh-ed25519", b"second")
        .unwrap();

    let entries = store.entries().unwrap();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries.first().unwrap().host, "first.test");
    assert_eq!(entries.first().unwrap().key, "AAAAfirst");
    store
        .verify("second.test", 22, "ssh-ed25519", b"second")
        .unwrap();
}

#[test]
fn a_marker_line_or_a_pattern_never_stands_for_a_host() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("known_hosts");
    std::fs::write(
        &path,
        "@revoked example.test ssh-ed25519 AAAAa\n\
         * ssh-ed25519 AAAAa\n\
         |1|c2FsdA==|aGFzaA== ssh-ed25519 AAAAa\n",
    )
    .unwrap();
    let store = KnownHosts::new(&path);
    // The marker line and the pattern name no host, and the third line's
    // digest is not the length the hashed form defines.
    assert!(store.entries().unwrap().is_empty());
    let error = store
        .verify("example.test", 22, "ssh-ed25519", b"key")
        .unwrap_err();
    assert!(matches!(error, VfsError::UnknownHostKey { .. }), "{error}");
}

// ------------------------------------------------------------------ transport

#[test]
fn a_fingerprint_that_is_not_a_digest_is_refused_rather_than_ignored() {
    for value in ["", "ab", "not-hex", &"ab".repeat(31), &"zz".repeat(32)] {
        let options = TlsOptions {
            pinned_fingerprints: vec![value.to_owned()],
            ..TlsOptions::default()
        };
        assert!(
            ca_vfs::remote::tls::client_config(&options).is_err(),
            "{value:?} was accepted as a fingerprint"
        );
    }
    let options = TlsOptions {
        pinned_fingerprints: vec!["AB:".repeat(32)],
        ..TlsOptions::default()
    };
    assert!(ca_vfs::remote::tls::client_config(&options).is_ok());
}

#[test]
fn an_unknown_field_cannot_turn_certificate_checking_off() {
    let profile = ca_vfs::remote::RemoteProfile {
        name: "share".to_owned(),
        service: ca_vfs::remote::ServiceProfile::WebDav(WebDavProfile {
            url: "https://example.test/share".to_owned(),
            ..WebDavProfile::default()
        }),
        ..ca_vfs::remote::RemoteProfile::default()
    };
    let mut value: serde_json::Value = serde_json::to_value(&profile).unwrap();
    // A build that does not know a field keeps it and acts on none of it.
    if let Some(map) = value.as_object_mut() {
        map.insert("verify_certificates".to_owned(), serde_json::json!(false));
        map.insert("accept_any_certificate".to_owned(), serde_json::json!(true));
        map.insert(
            "allow_plaintext_credentials".to_owned(),
            serde_json::json!(true),
        );
    }
    let back: ca_vfs::remote::RemoteProfile = serde_json::from_value(value).unwrap();
    let ca_vfs::remote::ServiceProfile::WebDav(settings) = back.service else {
        panic!("expected a share profile");
    };
    assert!(!settings.tls.accept_any_certificate);
    assert!(
        !ca_vfs::remote::TlsOptions::try_from(&settings.tls)
            .unwrap()
            .accept_any_certificate
    );
}
