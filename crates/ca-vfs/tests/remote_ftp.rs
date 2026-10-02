//! The file transfer protocol, against a server running in this process.

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
use std::sync::Arc;
use std::time::Duration;

use ca_vfs::remote::ftp::FtpFs;
use ca_vfs::remote::profile::{FtpProfile, FtpProtocol, TlsSettings};
use ca_vfs::remote::{MemorySecretStore, RemoteContext, SecretRef};
use ca_vfs::{Cancel, FileSystem, TimeFidelity, VfsError, VfsPath};

use ca_vfs::testing::ftp_server::{FtpTestServer, Options};

/// A folder with a small tree in it.
fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("report.txt"), b"hello remote world").unwrap();
    std::fs::create_dir(directory.path().join("sub")).unwrap();
    std::fs::write(
        directory.path().join("sub").join("inner.bin"),
        vec![7u8; 4096],
    )
    .unwrap();
    directory
}

/// A profile pointing at `server`.
fn profile(server: &FtpTestServer, options: Options) -> FtpProfile {
    let mut settings = FtpProfile::default();
    settings.login.host = "127.0.0.1".to_owned();
    settings.login.port = Some(server.port());
    settings.login.username = "operator".to_owned();
    settings.login.password = SecretRef::new("password");
    settings.listing.use_mlsd = options.mlsd;
    settings.connection.passive = true;
    settings
}

/// A context whose store holds the one password the server accepts.
fn context() -> RemoteContext {
    let store = Arc::new(MemorySecretStore::new());
    store.insert("password", "hunter2");
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(10);
    context
}

/// Start a server and connect to it.
fn connected(options: Options) -> (tempfile::TempDir, FtpTestServer, FtpFs) {
    let directory = fixture();
    let server = FtpTestServer::start(directory.path(), options.clone());
    let settings = profile(&server, options);
    let fs = FtpFs::connect(&settings, &context(), &Cancel::new()).expect("the server accepts");
    (directory, server, fs)
}

fn accounted(mlsd: bool) -> Options {
    Options {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        mlsd,
        ..Options::default()
    }
}

#[test]
fn a_listing_reports_names_sizes_and_times() {
    for mlsd in [false, true] {
        let (_directory, _server, fs) = connected(accounted(mlsd));
        let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"report.txt"), "{names:?}");
        assert!(names.contains(&"sub"), "{names:?}");

        let report = entries
            .iter()
            .find(|entry| entry.name == "report.txt")
            .unwrap();
        assert_eq!(report.size, 18);
        assert!(report.size_is_exact);
        assert!(report.modified.is_some());
        if mlsd {
            assert_eq!(report.time_fidelity, TimeFidelity::Utc);
        } else {
            assert_eq!(report.time_fidelity, TimeFidelity::MinutePrecision);
        }
    }
}

#[test]
fn a_nested_listing_reaches_the_whole_tree() {
    let (_directory, _server, fs) = connected(accounted(true));
    let walked = ca_vfs::walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
    let paths: Vec<String> = walked
        .iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect();
    assert!(paths.contains(&"sub/inner.bin".to_owned()), "{paths:?}");
}

#[test]
fn a_file_reads_back_byte_for_byte() {
    let (_directory, _server, fs) = connected(accounted(true));
    let mut handle = fs
        .open(&VfsPath::parse("report.txt").unwrap(), &Cancel::new())
        .unwrap();
    let mut text = String::new();
    handle.read_to_string(&mut text).unwrap();
    assert_eq!(text, "hello remote world");
}

#[test]
fn a_read_restarts_at_an_offset() {
    let (_directory, _server, fs) = connected(accounted(true));
    let mut handle = fs
        .open_at(&VfsPath::parse("report.txt").unwrap(), 6, &Cancel::new())
        .unwrap();
    let mut text = String::new();
    handle.read_to_string(&mut text).unwrap();
    assert_eq!(text, "remote world");
}

#[test]
fn a_write_reads_back_and_leaves_no_temporary_name() {
    let (directory, _server, fs) = connected(accounted(true));
    let path = VfsPath::parse("written.txt").unwrap();
    fs.write_file(&path, &mut b"written content".as_slice(), &Cancel::new())
        .unwrap();
    let on_disk = std::fs::read_to_string(directory.path().join("written.txt")).unwrap();
    assert_eq!(on_disk, "written content");

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
    let options = Options {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        mlsd: true,
        break_uploads: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let fs = FtpFs::connect(&profile(&server, options), &context(), &Cancel::new()).unwrap();

    let path = VfsPath::parse("report.txt").unwrap();
    let error = fs.write_file(&path, &mut b"replacement".as_slice(), &Cancel::new());
    assert!(error.is_err());
    let kept = std::fs::read_to_string(directory.path().join("report.txt")).unwrap();
    assert_eq!(kept, "hello remote world");
}

#[test]
fn rename_delete_and_create_folder_all_work() {
    let (directory, _server, fs) = connected(accounted(true));
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
    assert!(!directory.path().join("report.txt").exists());

    fs.delete(&VfsPath::parse("moved.txt").unwrap(), &cancel)
        .unwrap();
    assert!(!directory.path().join("moved.txt").exists());

    fs.delete(&VfsPath::parse("sub").unwrap(), &cancel).unwrap();
    assert!(!directory.path().join("sub").exists());
}

#[test]
fn a_hostile_listing_cannot_escape_the_root_or_end_the_walk() {
    for mlsd in [false, true] {
        let directory = fixture();
        let options = Options {
            account: Some(("operator".to_owned(), "hunter2".to_owned())),
            mlsd,
            hostile_listing: true,
            ..Options::default()
        };
        let server = FtpTestServer::start(directory.path(), options.clone());
        let fs = FtpFs::connect(&profile(&server, options), &context(), &Cancel::new()).unwrap();
        let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
        assert!(!entries.is_empty());
        for entry in &entries {
            assert_eq!(entry.path.depth(), 1, "{:?} left the root", entry.path);
            assert_eq!(entry.refused, entry.error.is_some(), "{entry:?}");
            assert!(VfsPath::parse(entry.path.as_str()).is_ok());
            assert!(!entry.name.contains('/'));
            assert!(!entry.name.contains('\\'));
            assert!(!entry.name.contains(':'));
            assert!(!entry.name.chars().any(char::is_control));
        }
        let flagged = entries.iter().filter(|entry| entry.error.is_some()).count();
        assert!(flagged >= 4, "the crafted names were not reported");
        assert!(
            entries.iter().any(|entry| entry.name == "ordinary.txt"),
            "an ordinary name was lost with the crafted ones"
        );
    }
}

#[test]
fn a_wrong_password_is_a_typed_error_with_no_secret_in_it() {
    let directory = fixture();
    let options = accounted(true);
    let server = FtpTestServer::start(directory.path(), options.clone());
    let mut settings = profile(&server, options);
    settings.login.password = SecretRef::new("wrong");
    let store = Arc::new(MemorySecretStore::new());
    store.insert("wrong", "not-the-password");
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(5);

    let Err(error) = FtpFs::connect(&settings, &context, &Cancel::new()) else {
        panic!("the wrong password must not log in");
    };
    assert!(matches!(error, VfsError::AuthFailed { .. }));
    let text = error.to_string();
    assert!(!text.contains("not-the-password"), "{text}");
    assert!(text.contains("operator"), "{text}");
}

#[test]
fn a_server_that_never_answers_times_out() {
    let directory = fixture();
    let options = Options {
        silent: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let settings = profile(&server, options);
    let mut context = context();
    context.call_timeout = Duration::from_millis(600);

    let started = std::time::Instant::now();
    let Err(error) = FtpFs::connect(&settings, &context, &Cancel::new()) else {
        panic!("a silent server must not connect");
    };
    assert!(matches!(error, VfsError::Timeout { .. }), "{error}");
    #[cfg(not(debug_assertions))]
    assert!(started.elapsed() < Duration::from_secs(10));
    let _ = started;
}

#[test]
fn a_cancel_stops_a_transfer_part_way() {
    let directory = fixture();
    let options = Options {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        mlsd: true,
        slow_transfer: Some(Duration::from_millis(20)),
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let fs = FtpFs::connect(&profile(&server, options), &context(), &Cancel::new()).unwrap();

    let cancel = Cancel::new();
    let mut handle = fs
        .open(&VfsPath::parse("sub/inner.bin").unwrap(), &cancel)
        .unwrap();
    let raised = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        raised.cancel();
    });
    let mut sink = Vec::new();
    let outcome = handle.read_to_end(&mut sink);
    assert!(outcome.is_err(), "a cancelled read must not complete");
    assert!(sink.len() < 4096);
}

#[test]
fn a_secured_channel_verifies_the_certificate_and_a_pin_accepts_it() {
    let directory = fixture();
    let options = Options {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        mlsd: true,
        explicit_tls: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let certificate = server.certificate.clone().expect("the server has one");

    let mut settings = profile(&server, options);
    settings.login.protocol = FtpProtocol::FtpsExplicit;
    settings.login.host = "localhost".to_owned();

    // The certificate is signed by nothing the trusted roots know, so the
    // connection is refused until the profile names it.
    let Err(error) = FtpFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("an unverified certificate must not be accepted");
    };
    assert!(matches!(
        error,
        VfsError::Tls { .. } | VfsError::Io(_) | VfsError::Network { .. }
    ));

    settings.connection.pinned_certificate_fingerprints = vec![certificate.fingerprint()];
    let fs = FtpFs::connect(&settings, &context(), &Cancel::new())
        .expect("the pinned certificate is accepted");
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert!(entries.iter().any(|entry| entry.name == "report.txt"));
}

#[test]
fn a_profile_that_accepts_any_certificate_is_the_only_way_past_validation() {
    let directory = fixture();
    let options = Options {
        mlsd: true,
        explicit_tls: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let mut settings = profile(&server, options);
    settings.login.protocol = FtpProtocol::FtpsExplicit;
    settings.login.host = "127.0.0.1".to_owned();
    assert!(!TlsSettings::default().accept_any_certificate);

    assert!(FtpFs::connect(&settings, &context(), &Cancel::new()).is_err());
    settings.connection.accept_any_certificate = true;
    let fs = FtpFs::connect(&settings, &context(), &Cancel::new())
        .expect("the explicit unsafe flag lets the connection through");
    assert!(fs.capabilities().writable);
}

#[test]
fn a_data_channel_resumes_the_control_channel_session() {
    let directory = fixture();
    let options = Options {
        mlsd: true,
        explicit_tls: true,
        require_data_resumption: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let mut settings = profile(&server, options);
    settings.login.protocol = FtpProtocol::FtpsExplicit;
    settings.login.host = "127.0.0.1".to_owned();
    settings.connection.accept_any_certificate = true;

    let fs = FtpFs::connect(&settings, &context(), &Cancel::new()).expect("connect");
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).expect("listing");
    assert!(entries.iter().any(|entry| entry.name == "report.txt"));

    let path = VfsPath::parse("report.txt").unwrap();
    let mut handle = fs.open_at(&path, 0, &Cancel::new()).expect("open");
    let mut body = Vec::new();
    handle.read_to_end(&mut body).expect("read");
    assert_eq!(body, b"hello remote world");
}

#[test]
fn a_server_that_refuses_to_protect_the_data_channel_is_a_transport_security_error() {
    let directory = fixture();
    let options = Options {
        mlsd: true,
        explicit_tls: true,
        refuse_data_protection: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let mut settings = profile(&server, options);
    settings.login.protocol = FtpProtocol::FtpsExplicit;
    settings.connection.accept_any_certificate = true;

    let Err(error) = FtpFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("a transfer must not run over a data channel the server left clear");
    };
    assert!(matches!(error, VfsError::Tls { .. }), "{error}");
}

#[test]
fn a_secured_profile_with_no_transport_configuration_is_refused() {
    use ca_vfs::remote::ftp::session::Session;
    use ca_vfs::remote::Deadline;

    let directory = fixture();
    let options = Options {
        explicit_tls: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    for protocol in [FtpProtocol::FtpsExplicit, FtpProtocol::FtpsImplicit] {
        let mut settings = profile(&server, options.clone());
        settings.login.protocol = protocol.clone();
        let outcome = Session::open(
            &settings,
            None,
            None,
            Deadline::after(Duration::from_secs(5)),
            &Cancel::new(),
        );
        assert!(
            matches!(outcome, Err(VfsError::Tls { .. })),
            "{protocol:?}: {outcome:?}"
        );
    }
}

#[test]
fn a_transfer_that_keeps_moving_outlasts_the_call_timeout() {
    let directory = fixture();
    let options = Options {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        mlsd: true,
        slow_transfer: Some(Duration::from_millis(100)),
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let mut context = context();
    context.call_timeout = Duration::from_millis(1_000);
    let fs = FtpFs::connect(&profile(&server, options), &context, &Cancel::new()).unwrap();

    // Eighteen bytes at one byte per interval take longer than the call
    // timeout, while no single wait comes near it.
    let started = std::time::Instant::now();
    let mut handle = fs
        .open(&VfsPath::parse("report.txt").unwrap(), &Cancel::new())
        .unwrap();
    let mut body = Vec::new();
    handle
        .read_to_end(&mut body)
        .expect("a moving transfer is not cut");
    assert_eq!(body, b"hello remote world");
    assert!(started.elapsed() > Duration::from_millis(1_000));
}

#[test]
fn a_refused_rename_into_place_leaves_the_old_file_untouched() {
    let directory = fixture();
    let options = Options {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        mlsd: true,
        refuse_renames: true,
        ..Options::default()
    };
    let server = FtpTestServer::start(directory.path(), options.clone());
    let fs = FtpFs::connect(&profile(&server, options), &context(), &Cancel::new()).unwrap();

    let path = VfsPath::parse("report.txt").unwrap();
    let outcome = fs.write_file(&path, &mut b"replacement".as_slice(), &Cancel::new());
    assert!(outcome.is_err());
    let kept = std::fs::read_to_string(directory.path().join("report.txt")).unwrap();
    assert_eq!(kept, "hello remote world");
}
