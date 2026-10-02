//! The secure shell transfer protocol, against a server running in this
//! process.

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
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ca_vfs::remote::hostkeys::ssh_fingerprint;
use ca_vfs::remote::profile::{FtpProfile, FtpProtocol};
use ca_vfs::remote::sftp::SftpFs;
use ca_vfs::remote::{KnownHosts, MemorySecretStore, RemoteContext, SecretRef};
use ca_vfs::{Cancel, FileSystem, TimeFidelity, VfsError, VfsPath};

use ca_vfs::testing::ssh_server::SshTestServer;

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

/// A profile pointing at `server`.
fn profile(server: &SshTestServer, known_hosts: &Path) -> FtpProfile {
    let mut settings = FtpProfile::default();
    settings.login.protocol = FtpProtocol::Sftp;
    settings.login.host = "127.0.0.1".to_owned();
    settings.login.port = Some(server.port());
    settings.login.username = "operator".to_owned();
    settings.login.password = SecretRef::new("password");
    settings.global.known_hosts_file = known_hosts.to_string_lossy().into_owned();
    settings
}

/// A context with the one password the server accepts.
fn context() -> RemoteContext {
    let store = Arc::new(MemorySecretStore::new());
    store.insert("password", "hunter2");
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(20);
    context
}

/// A server, a store that already trusts it, and a connected file system.
fn connected() -> (tempfile::TempDir, tempfile::TempDir, SshTestServer, SftpFs) {
    let directory = fixture();
    let settings_dir = tempfile::tempdir().unwrap();
    let server = SshTestServer::start(directory.path(), "operator", "hunter2");
    let store = KnownHosts::new(settings_dir.path().join("known_hosts"));
    store
        .record(
            "127.0.0.1",
            server.port(),
            &server.host_key_type,
            &server.host_key,
        )
        .unwrap();
    let settings = profile(&server, &settings_dir.path().join("known_hosts"));
    let fs = SftpFs::connect(&settings, &context(), &Cancel::new()).expect("the server accepts");
    (directory, settings_dir, server, fs)
}

#[test]
fn an_unrecorded_host_key_is_reported_with_its_fingerprint() {
    let directory = fixture();
    let settings_dir = tempfile::tempdir().unwrap();
    let server = SshTestServer::start(directory.path(), "operator", "hunter2");
    let settings = profile(&server, &settings_dir.path().join("known_hosts"));

    let Err(error) = SftpFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("an unrecorded host key must not be accepted");
    };
    let VfsError::UnknownHostKey { host, fingerprint } = error else {
        panic!("the wrong error was reported");
    };
    assert!(host.contains("127.0.0.1"));
    assert_eq!(fingerprint, ssh_fingerprint(&server.host_key));
}

#[test]
fn a_changed_host_key_is_a_hard_error() {
    let directory = fixture();
    let settings_dir = tempfile::tempdir().unwrap();
    let server = SshTestServer::start(directory.path(), "operator", "hunter2");
    let path = settings_dir.path().join("known_hosts");
    let store = KnownHosts::new(&path);
    store
        .record(
            "127.0.0.1",
            server.port(),
            &server.host_key_type,
            b"a different key",
        )
        .unwrap();

    let settings = profile(&server, &path);
    let Err(error) = SftpFs::connect(&settings, &context(), &Cancel::new()) else {
        panic!("a changed host key must not be accepted");
    };
    assert!(matches!(error, VfsError::HostKeyChanged { .. }), "{error}");
}

#[test]
fn a_listing_reports_names_sizes_and_times() {
    let (_directory, _settings, _server, fs) = connected();
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
    let (_directory, _settings, _server, fs) = connected();
    let walked = ca_vfs::walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
    assert!(walked
        .iter()
        .any(|entry| entry.path.as_str() == "sub/inner.bin"));
}

#[test]
fn a_file_reads_back_and_a_read_resumes_at_an_offset() {
    let (_directory, _settings, _server, fs) = connected();
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
    let (directory, _settings, _server, fs) = connected();
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
    let (directory, _settings, _server, fs) = connected();
    let path = VfsPath::parse("report.txt").unwrap();

    /// A reader that fails part way through, standing in for a source that
    /// goes away mid-transfer.
    struct Failing(usize);
    impl Read for Failing {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("the source went away"));
            }
            self.0 -= 1;
            let take = buf.len().min(8);
            let Some(slice) = buf.get_mut(..take) else {
                return Ok(0);
            };
            slice.fill(b'x');
            Ok(take)
        }
    }

    assert!(fs
        .write_file(&path, &mut Failing(2), &Cancel::new())
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
    let (directory, _settings, _server, fs) = connected();
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

    fs.delete(&VfsPath::parse("sub").unwrap(), &cancel).unwrap();
    assert!(!directory.path().join("sub").exists());
}

#[test]
fn a_hostile_name_in_a_listing_cannot_escape_the_root() {
    let (directory, _settings, _server, fs) = connected();
    // The server cannot be made to report a separator in a name, so the
    // names that a file system does accept are created and the listing is
    // checked for anything that leaves the root.
    for name in ["..dots", "Report.TXT", "spaced name"] {
        std::fs::write(directory.path().join(name), b"x").unwrap();
    }
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    for entry in &entries {
        assert_eq!(entry.path.depth(), 1, "{:?} left the root", entry.path);
        assert!(VfsPath::parse(entry.path.as_str()).is_ok());
    }
    assert!(entries.iter().any(|entry| entry.name == "..dots"));
}

/// A mapped spelling must not be treated as another readable server item.
#[test]
fn a_mapped_sftp_name_is_refused_in_the_listing() {
    let (directory, _settings, _server, fs) = connected();
    let exact = std::fs::canonicalize(directory.path()).unwrap();
    std::fs::write(exact.join("trailing."), b"original").unwrap();
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    let row = entries
        .iter()
        .find(|entry| entry.name == "trailing_")
        .unwrap();
    assert!(row.refused, "{row:?}");
    assert!(row.error.as_deref().unwrap().contains("trailing."));
    assert!(entries
        .iter()
        .any(|entry| entry.name == "report.txt" && !entry.refused));
}

#[test]
fn a_wrong_password_is_a_typed_error_with_no_secret_in_it() {
    let directory = fixture();
    let settings_dir = tempfile::tempdir().unwrap();
    let server = SshTestServer::start(directory.path(), "operator", "hunter2");
    let path = settings_dir.path().join("known_hosts");
    KnownHosts::new(&path)
        .record(
            "127.0.0.1",
            server.port(),
            &server.host_key_type,
            &server.host_key,
        )
        .unwrap();
    let mut settings = profile(&server, &path);
    settings.login.password = SecretRef::new("wrong");
    let store = Arc::new(MemorySecretStore::new());
    store.insert("wrong", "not-the-password");
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(10);

    let Err(error) = SftpFs::connect(&settings, &context, &Cancel::new()) else {
        panic!("the wrong password must not log in");
    };
    assert!(matches!(error, VfsError::AuthFailed { .. }), "{error}");
    assert!(!error.to_string().contains("not-the-password"));
}

#[test]
fn a_cancel_stops_a_read() {
    let (_directory, _settings, _server, fs) = connected();
    let cancel = Cancel::new();
    let mut handle = fs
        .open(&VfsPath::parse("sub/inner.bin").unwrap(), &cancel)
        .unwrap();
    cancel.cancel();
    let mut sink = Vec::new();
    assert!(handle.read_to_end(&mut sink).is_err());
    assert!(sink.is_empty());
}

#[test]
fn a_host_that_is_not_listening_reports_a_network_error() {
    let settings_dir = tempfile::tempdir().unwrap();
    let mut settings = FtpProfile::default();
    settings.login.protocol = FtpProtocol::Sftp;
    settings.login.host = "127.0.0.1".to_owned();
    // Port 1 is reserved and nothing in the test suite listens on it.
    settings.login.port = Some(1);
    settings.global.known_hosts_file = settings_dir
        .path()
        .join("known_hosts")
        .to_string_lossy()
        .into_owned();
    let mut context = context();
    context.call_timeout = Duration::from_secs(3);

    let Err(error) = SftpFs::connect(&settings, &context, &Cancel::new()) else {
        panic!("a closed port must not connect");
    };
    assert!(
        matches!(error, VfsError::Network { .. } | VfsError::Timeout { .. }),
        "{error}"
    );
}
