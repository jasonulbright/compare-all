//! One side of a comparison reached over a protocol, end to end.
//!
//! Every server runs in this process, binds the loopback address on a port the
//! operating system picks and serves a temporary folder. Nothing reaches the
//! network, no real user folder is touched and no credential of the machine is
//! read.
//!
//! Each protocol runs the same four checks: scan the remote side, settle it
//! against a local side with the quick tests, copy a file each way, and prove
//! that an interrupted upload leaves the old remote file as it was.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ca_fs::{
    quick_compare_with, scan_source, Cancel, FileOps, Mount, QuickTests, Source, SourceKind,
    SourceOps,
};
use ca_vfs::remote::profile::{
    FtpProfile, FtpProtocol, S3Auth, S3Profile, TlsSettings, WebDavProfile,
};
use ca_vfs::testing::ftp_server::{FtpTestServer, Options as FtpOptions};
use ca_vfs::testing::s3_server::{Behaviour as S3Behaviour, S3TestServer, BUCKET};
use ca_vfs::testing::ssh_server::SshTestServer;
use ca_vfs::testing::webdav_server::{Behaviour as WebDavBehaviour, WebDavTestServer};
use ca_vfs::{
    Cancel as VfsCancel, FileSystem, KnownHosts, MemorySecretStore, RemoteContext, SecretRef,
};

/// The one file both sides hold, and the bytes it holds.
const SHARED_NAME: &str = "report.txt";
/// Content of the shared file.
const SHARED: &[u8] = b"hello remote world";

/// A folder the server serves.
fn remote_fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(SHARED_NAME), SHARED).unwrap();
    directory
}

/// A local folder holding the same file, plus one the remote side lacks.
fn local_fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(SHARED_NAME), SHARED).unwrap();
    std::fs::write(directory.path().join("local-only.txt"), b"local only").unwrap();
    directory
}

/// A context holding the one secret every server here accepts.
fn context(id: &str, value: &str) -> RemoteContext {
    let store = Arc::new(MemorySecretStore::new());
    store.insert(id, value);
    let mut context = RemoteContext::with_secrets(store);
    context.call_timeout = Duration::from_secs(20);
    context
}

/// Run the four checks against one connected file system.
fn check_protocol(label: &str, remote_root: &Path, remote_fs: Arc<dyn FileSystem>) {
    let local_dir = local_fixture();
    let local_root = local_dir.path();

    // The two fixtures are written at different moments. Under load the gap
    // can exceed the precision of a listing, so both files get one fixed time
    // on a whole minute, which every listing precision can state exactly.
    let stamp = filetime::FileTime::from_unix_time(1_577_836_800, 0);
    filetime::set_file_mtime(local_root.join(SHARED_NAME), stamp).unwrap();
    filetime::set_file_mtime(remote_root.join(SHARED_NAME), stamp).unwrap();

    let remote = Source::over(SourceKind::Remote, remote_fs);

    // 1. The remote side scans.
    let listed = scan_source(
        &remote,
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap_or_else(|error| panic!("{label}: the remote side does not scan: {error}"));
    let names: Vec<&str> = listed
        .entries
        .values()
        .map(|entry| entry.name.as_str())
        .collect();
    assert!(names.contains(&SHARED_NAME), "{label}: {names:?}");

    // 2. The quick tests settle the shared file against the local one, at the
    //    precision the remote listing states.
    let local = scan_source(
        &Source::local(local_root),
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    let rel = Path::new(SHARED_NAME);
    let result = quick_compare_with(
        &local.entries[rel],
        local.facts_of(rel),
        &listed.entries[rel],
        listed.facts_of(rel),
        &QuickTests {
            timestamp: true,
            ..QuickTests::default()
        },
    );
    assert!(
        result.is_same(),
        "{label}: the two sides do not settle: {result:?}"
    );

    // 3. A copy in each direction.
    let mount: PathBuf = local_root.join("remote-mount");
    let ops = SourceOps::new(vec![Mount::new(mount.clone(), remote.clone())]);

    let uploaded = mount.join("uploaded.txt");
    let mut writer = ops.create_new(&uploaded).unwrap();
    writer.write_all(b"from the local side").unwrap();
    writer.sync_data().unwrap();
    drop(writer);
    assert_eq!(
        std::fs::read(remote_root.join("uploaded.txt")).unwrap(),
        b"from the local side",
        "{label}: the upload did not land"
    );

    let mut reader = ops.open_read(&mount.join(SHARED_NAME)).unwrap();
    let mut downloaded = Vec::new();
    reader.read_to_end(&mut downloaded).unwrap();
    drop(reader);
    assert_eq!(downloaded, SHARED, "{label}: the download differs");

    // 4. An upload that is never flushed leaves the old remote file as it was.
    //    A copy writes under a temporary name first, which is the name the
    //    interrupted step holds.
    let target = mount.join(format!("{SHARED_NAME}.part"));
    let mut writer = ops.create_new(&target).unwrap();
    writer.write_all(b"half").unwrap();
    // The writer goes away without a flush, which is what an interrupted step
    // looks like.
    drop(writer);
    assert_eq!(
        std::fs::read(remote_root.join(SHARED_NAME)).unwrap(),
        SHARED,
        "{label}: an interrupted upload replaced the old file"
    );
    assert!(
        !remote_root.join(format!("{SHARED_NAME}.part")).exists(),
        "{label}: an interrupted upload left a part written name behind"
    );

    // The content test reads both sides through their own sources.
    let path = ca_vfs::VfsPath::parse(SHARED_NAME).unwrap();
    let outcome = ca_fs::compare_source_contents(
        side(&Source::local(local_root), &path),
        side(&remote, &path),
        &ca_fs::ContentTests::default(),
        &Cancel::new(),
    )
    .unwrap_or_else(|error| panic!("{label}: the content test failed: {error}"));
    assert_eq!(
        outcome,
        ca_fs::ContentOutcome::BinarySame,
        "{label}: the shared file differs"
    );
}

/// One side of a content test.
fn side<'a>(source: &'a Source, path: &'a ca_vfs::VfsPath) -> ca_fs::ContentSide<'a> {
    ca_fs::ContentSide {
        source,
        path,
        facts: ca_fs::EntryFacts::default(),
    }
}

#[test]
fn a_file_transfer_side_scans_compares_and_copies() {
    let remote_dir = remote_fixture();
    let options = FtpOptions {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        mlsd: true,
        ..FtpOptions::default()
    };
    let server = FtpTestServer::start(remote_dir.path(), options);
    let mut settings = FtpProfile::default();
    settings.login.host = "127.0.0.1".to_owned();
    settings.login.port = Some(server.port());
    settings.login.username = "operator".to_owned();
    settings.login.password = SecretRef::new("password");
    settings.listing.use_mlsd = true;
    settings.connection.passive = true;
    let fs = ca_vfs::remote::ftp::FtpFs::connect(
        &settings,
        &context("password", "hunter2"),
        &VfsCancel::new(),
    )
    .expect("the server accepts");
    check_protocol("file transfer", remote_dir.path(), Arc::new(fs));
}

#[test]
fn a_secure_shell_side_scans_compares_and_copies() {
    let remote_dir = remote_fixture();
    let settings_dir = tempfile::tempdir().unwrap();
    let server = SshTestServer::start(remote_dir.path(), "operator", "hunter2");
    let known_hosts = settings_dir.path().join("known_hosts");
    KnownHosts::new(&known_hosts)
        .record(
            "127.0.0.1",
            server.port(),
            &server.host_key_type,
            &server.host_key,
        )
        .unwrap();
    let mut settings = FtpProfile::default();
    settings.login.protocol = FtpProtocol::Sftp;
    settings.login.host = "127.0.0.1".to_owned();
    settings.login.port = Some(server.port());
    settings.login.username = "operator".to_owned();
    settings.login.password = SecretRef::new("password");
    settings.global.known_hosts_file = known_hosts.to_string_lossy().into_owned();
    let fs = ca_vfs::remote::sftp::SftpFs::connect(
        &settings,
        &context("password", "hunter2"),
        &VfsCancel::new(),
    )
    .expect("the server accepts");
    check_protocol("secure shell", remote_dir.path(), Arc::new(fs));
}

#[test]
fn an_http_share_side_scans_compares_and_copies() {
    let remote_dir = remote_fixture();
    let server = WebDavTestServer::start(remote_dir.path(), WebDavBehaviour::default());
    let settings = WebDavProfile {
        url: format!("http://127.0.0.1:{}", server.port()),
        timeout_seconds: Some(20),
        // The server in this process speaks plain HTTP on the loopback
        // address, which is the one case the refusal has to be waived for.
        allow_plaintext_credentials: true,
        ..WebDavProfile::default()
    };
    let fs = ca_vfs::remote::webdav::WebDavFs::connect(
        &settings,
        &context("password", "hunter2"),
        &VfsCancel::new(),
    )
    .expect("the server answers");
    check_protocol("http share", remote_dir.path(), Arc::new(fs));
}

#[test]
fn an_object_store_side_scans_compares_and_copies() {
    let remote_dir = remote_fixture();
    let server = S3TestServer::start(remote_dir.path(), S3Behaviour::default());
    let settings = S3Profile {
        auth: S3Auth::Saved {
            access_key_id: "AKIAEXAMPLE".to_owned(),
            secret_access_key: SecretRef::new("secret"),
            session_token: SecretRef::default(),
            unknown: std::collections::BTreeMap::default(),
        },
        bucket: BUCKET.to_owned(),
        region: "us-east-1".to_owned(),
        endpoint: format!("http://127.0.0.1:{}", server.port()),
        path_style: true,
        timeout_seconds: Some(20),
        tls: TlsSettings::default(),
        ..S3Profile::default()
    };
    let fs = ca_vfs::remote::s3::S3Fs::connect(
        &settings,
        &context("secret", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
        &VfsCancel::new(),
    )
    .expect("the store answers");
    check_protocol("object store", remote_dir.path(), Arc::new(fs));
}

/// A plain transfer listing states each clock in the zone of the server, and
/// the profile names that zone. The reader places the clock on the UTC line
/// once, and a scan of the side takes the stamp as placed: it lands on the
/// instant the file was written.
#[test]
fn a_transfer_listing_stamp_is_placed_once() {
    let remote_dir = tempfile::tempdir().unwrap();
    // A whole minute ten days ago, which a plain listing states as a clock
    // with no year.
    let now = ca_vfs::remote::timestamp::unix_seconds(std::time::SystemTime::now());
    let written = (now - 10 * 86_400) / 60 * 60;
    let file = remote_dir.path().join(SHARED_NAME);
    std::fs::write(&file, SHARED).unwrap();
    // The server here prints the clock of each file time as it stands, which
    // is what a server 90 minutes east prints for an instant 90 minutes
    // earlier.
    filetime::set_file_mtime(
        &file,
        filetime::FileTime::from_unix_time(written + 5_400, 0),
    )
    .unwrap();
    let options = FtpOptions {
        account: Some(("operator".to_owned(), "hunter2".to_owned())),
        ..FtpOptions::default()
    };
    let server = FtpTestServer::start(remote_dir.path(), options);
    let mut settings = FtpProfile::default();
    settings.login.host = "127.0.0.1".to_owned();
    settings.login.port = Some(server.port());
    settings.login.username = "operator".to_owned();
    settings.login.password = SecretRef::new("password");
    settings.listing.use_mlsd = false;
    settings.connection.passive = true;
    settings.server.time_zone_offset_minutes = 90;
    let fs = ca_vfs::remote::ftp::FtpFs::connect(
        &settings,
        &context("password", "hunter2"),
        &VfsCancel::new(),
    )
    .expect("the server accepts");

    let remote = Source::over(SourceKind::Remote, Arc::new(fs));
    let listed = scan_source(
        &remote,
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    let rel = Path::new(SHARED_NAME);
    assert_eq!(
        listed.facts_of(rel).time_fidelity,
        ca_fs::TimeFidelity::MinutePrecision
    );
    assert_eq!(
        listed.entries[rel].modified,
        Some(ca_vfs::remote::timestamp::system_time(written))
    );
}
