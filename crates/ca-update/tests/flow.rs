//! The check, download, verification and apply steps against a local server.
//! Nothing here reaches the internet.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_update::apply::{apply_portable_with, apply_staged, Trust};
use ca_update::flow::{run, Endpoints, Job, Outcome};
use ca_update::manifest::{msi_name, zip_name, Manifest, ManifestAsset, MANIFEST_NAME, SCHEMA};
use ca_update::mode::InstallMode;
use ca_update::net::{HttpTransport, ProxyConfig};
use ca_update::signature::{SignatureVerifier, SignerFacts};
use ca_update::staging::Staging;
use ca_update::verify::hex;
use ca_update::Version;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[path = "support/pe.rs"]
mod fixture;
use fixture::pe;

const CURRENT: &str = "2026.10.01.0001";
const NEXT: &str = "2026.10.18.0007";
const SIGNER: &str = "Jason Ulbright";

type Routes = Arc<Mutex<HashMap<String, (u16, String, Vec<u8>)>>>;

struct Server {
    base: String,
    routes: Routes,
    hits: Arc<AtomicUsize>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let hits = Arc::new(AtomicUsize::new(0));
        let (r, h) = (Arc::clone(&routes), Arc::clone(&hits));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (r, h) = (Arc::clone(&r), Arc::clone(&h));
                std::thread::spawn(move || {
                    let mut buffer = [0u8; 4096];
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    h.fetch_add(1, Ordering::SeqCst);
                    let found = r.lock().unwrap().get(&path).cloned();
                    let (status, kind, body) =
                        found.unwrap_or((404, "text/plain".to_owned(), b"missing".to_vec()));
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                });
            }
        });
        Self { base, routes, hits }
    }

    fn serve(&self, path: &str, status: u16, kind: &str, body: Vec<u8>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, kind.to_owned(), body));
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            latest: format!("{}/latest", self.base),
            download_base: format!("{}/download", self.base),
            page_base: format!("{}/tag", self.base),
        }
    }
}

struct FakeSigner(Mutex<Result<String, String>>);

impl FakeSigner {
    fn signed_by(name: &str) -> Self {
        Self(Mutex::new(Ok(format!("CN={name}, C=US"))))
    }
}

impl SignatureVerifier for FakeSigner {
    fn signer_facts(&self, _: &Path) -> Result<SignerFacts, String> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .map(|subject| SignerFacts::release(&subject))
    }
}

fn running() -> Version {
    Version::parse(CURRENT).unwrap()
}

const UPGRADE_CODE: &str = "{6F1C2A4E-3B7D-4E59-9A21-C0DE0A11C0A1}";

/// An installer database whose `Property` table names `upgrade_code` and
/// `product_version`.
fn msi_package(upgrade_code: &str, product_version: &str) -> Vec<u8> {
    let cursor = std::io::Cursor::new(Vec::new());
    let mut package = msi::Package::create(msi::PackageType::Installer, cursor).unwrap();
    package
        .create_table(
            "Property",
            vec![
                msi::Column::build("Property").primary_key().id_string(72),
                msi::Column::build("Value").text_string(0),
            ],
        )
        .unwrap();
    let rows = [
        ("ProductName", "Compare All"),
        ("Manufacturer", SIGNER),
        ("UpgradeCode", upgrade_code),
        ("ProductVersion", product_version),
    ];
    package
        .insert_rows(
            msi::Insert::into("Property").rows(
                rows.iter()
                    .map(|(name, value)| vec![msi::Value::from(*name), msi::Value::from(*value)])
                    .collect(),
            ),
        )
        .unwrap();
    package.into_inner().unwrap().into_inner()
}

/// The installer of [`NEXT`].
fn msi_bytes() -> Vec<u8> {
    msi_package(UPGRADE_CODE, "26.10.7")
}

fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn publish(server: &Server, name: &str, bytes: &[u8], kind: &str) -> ManifestAsset {
    server.serve(
        &format!("/download/v{NEXT}/{name}"),
        200,
        kind,
        bytes.to_vec(),
    );
    ManifestAsset {
        name: name.to_owned(),
        size: bytes.len() as u64,
        sha256: sha(bytes),
    }
}

fn publish_manifest(server: &Server, assets: Vec<ManifestAsset>) {
    let manifest = Manifest {
        schema: SCHEMA,
        version: NEXT.to_owned(),
        minimum_updater_version: 1,
        assets,
    };
    server.serve(
        &format!("/download/v{NEXT}/{MANIFEST_NAME}"),
        200,
        "application/json",
        serde_json::to_vec(&manifest).unwrap(),
    );
    server.serve(
        "/latest",
        200,
        "application/json",
        format!(r#"{{"tag_name":"v{NEXT}"}}"#).into_bytes(),
    );
}

fn transport() -> HttpTransport {
    HttpTransport::new(
        "compare-all/test".to_owned(),
        ProxyConfig::direct(),
        Duration::from_secs(10),
    )
    .allowing_plain_http()
}

fn check(
    server: &Server,
    staging: &Staging,
    signer: &FakeSigner,
    mode: Option<InstallMode>,
) -> Result<Outcome, String> {
    let endpoints = server.endpoints();
    let cancel = Arc::new(AtomicBool::new(false));
    let transport = transport();
    run(&Job {
        transport: &transport,
        signer,
        staging,
        mode,
        current: Version::parse(CURRENT).unwrap(),
        endpoints: &endpoints,
        signer_name: SIGNER,
        cancel: &cancel,
    })
}

fn staging_dir() -> (tempfile::TempDir, Staging) {
    let dir = tempfile::tempdir().unwrap();
    let staging = Staging::new(dir.path().join("updates"));
    (dir, staging)
}

fn assert_failed_and_clean(outcome: &Outcome, staging: &Staging, needle: &str) {
    let Outcome::DownloadFailed {
        version,
        page,
        reason,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(version.to_string(), NEXT);
    assert!(page.ends_with(&format!("/tag/v{NEXT}")), "{page}");
    assert!(reason.contains(needle), "{reason}");
    assert!(staging.read_record().is_none());
    assert_eq!(staging.read_warning().unwrap().version, NEXT);
    let leftovers: Vec<_> = std::fs::read_dir(staging.dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, vec!["download-warning.json".to_owned()]);
}

#[test]
fn a_verified_installer_is_staged() {
    let server = Server::start();
    let msi = msi_bytes();
    let asset = publish(&server, &msi_name(NEXT), &msi, "application/x-msi");
    publish_manifest(&server, vec![asset]);
    let (_keep, staging) = staging_dir();
    let outcome = check(
        &server,
        &staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Installed { machine: false }),
    )
    .unwrap();
    assert_eq!(
        outcome,
        Outcome::Staged {
            version: Version::parse(NEXT).unwrap()
        }
    );
    let record = staging.read_record().unwrap();
    assert!(record.verified);
    assert_eq!(std::fs::read(staging.asset_path(&record)).unwrap(), msi);
    assert!(staging.read_warning().is_none());
}

#[test]
fn an_html_page_instead_of_the_asset_fails_the_download_only() {
    let server = Server::start();
    let msi = msi_bytes();
    let asset = publish(&server, &msi_name(NEXT), &msi, "application/x-msi");
    publish_manifest(&server, vec![asset.clone()]);
    let page = b"<html>blocked by policy</html>".to_vec();
    server.serve(
        &format!("/download/v{NEXT}/{}", asset.name),
        200,
        "text/html; charset=utf-8",
        page.clone(),
    );
    let (_keep, staging) = staging_dir();
    let outcome = check(
        &server,
        &staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Installed { machine: true }),
    )
    .unwrap();
    assert_failed_and_clean(&outcome, &staging, "not a file");

    // The same page under a file content type is caught by size or format.
    server.serve(
        &format!("/download/v{NEXT}/{}", asset.name),
        200,
        "application/octet-stream",
        vec![b'<'; msi.len()],
    );
    let outcome = check(
        &server,
        &staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Installed { machine: true }),
    )
    .unwrap();
    assert_failed_and_clean(&outcome, &staging, "web page");
}

#[test]
fn a_wrong_size_or_checksum_or_signer_fails_the_download() {
    let server = Server::start();
    let msi = msi_bytes();
    let asset = publish(&server, &msi_name(NEXT), &msi, "application/x-msi");
    let mode = Some(InstallMode::Installed { machine: false });
    let route = format!("/download/v{NEXT}/{}", asset.name);

    publish_manifest(&server, vec![asset.clone()]);
    let mut longer = msi.clone();
    longer.push(0);
    server.serve(&route, 200, "application/x-msi", longer);
    let (_k1, staging) = staging_dir();
    let outcome = check(&server, &staging, &FakeSigner::signed_by(SIGNER), mode).unwrap();
    assert_failed_and_clean(&outcome, &staging, "larger");

    let mut shorter = msi.clone();
    shorter.pop();
    server.serve(&route, 200, "application/x-msi", shorter);
    let (_k2, staging) = staging_dir();
    let outcome = check(&server, &staging, &FakeSigner::signed_by(SIGNER), mode).unwrap();
    assert_failed_and_clean(&outcome, &staging, "bytes");

    let mut altered = msi.clone();
    altered[100] ^= 1;
    server.serve(&route, 200, "application/x-msi", altered);
    let (_k3, staging) = staging_dir();
    let outcome = check(&server, &staging, &FakeSigner::signed_by(SIGNER), mode).unwrap();
    assert_failed_and_clean(&outcome, &staging, "SHA-256");

    server.serve(&route, 200, "application/x-msi", msi);
    let (_k4, staging) = staging_dir();
    let outcome = check(
        &server,
        &staging,
        &FakeSigner::signed_by("Someone Else"),
        mode,
    )
    .unwrap();
    assert_failed_and_clean(&outcome, &staging, "signed by");

    let unsigned = FakeSigner(Mutex::new(Err("not signed".to_owned())));
    let (_k5, staging) = staging_dir();
    let outcome = check(&server, &staging, &unsigned, mode).unwrap();
    assert_failed_and_clean(&outcome, &staging, "not signed");
}

#[test]
fn a_blocked_download_after_a_good_check_leaves_the_launch_warning() {
    let server = Server::start();
    let msi = msi_bytes();
    let asset = publish(&server, &msi_name(NEXT), &msi, "application/x-msi");
    publish_manifest(&server, vec![asset.clone()]);
    server.serve(
        &format!("/download/v{NEXT}/{}", asset.name),
        403,
        "text/html",
        b"blocked".to_vec(),
    );
    let (_keep, staging) = staging_dir();
    let outcome = check(
        &server,
        &staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Installed { machine: false }),
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::DownloadFailed { .. }));
    let warning = staging.read_warning().unwrap();
    assert_eq!(warning.version, NEXT);
    assert!(warning.page.ends_with(&format!("/tag/v{NEXT}")));
}

#[test]
fn an_unreadable_manifest_is_a_failed_check_not_a_download_failure() {
    let server = Server::start();
    server.serve(
        "/latest",
        200,
        "application/json",
        format!(r#"{{"tag_name":"v{NEXT}"}}"#).into_bytes(),
    );
    let (_keep, staging) = staging_dir();
    let result = check(
        &server,
        &staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Portable),
    );
    assert!(result.is_err());
    assert!(staging.read_warning().is_none());
}

#[test]
fn the_current_build_is_current_and_clears_old_state() {
    let server = Server::start();
    server.serve(
        "/latest",
        200,
        "application/json",
        format!(r#"{{"tag_name":"v{CURRENT}"}}"#).into_bytes(),
    );
    let (_keep, staging) = staging_dir();
    staging
        .write_warning(&ca_update::staging::DownloadWarning::new(
            &Version::parse(CURRENT).unwrap(),
            "https://example.test/x".to_owned(),
        ))
        .unwrap();
    let outcome = check(&server, &staging, &FakeSigner::signed_by(SIGNER), None).unwrap();
    assert_eq!(outcome, Outcome::Current);
    assert!(staging.read_warning().is_none());
}

#[test]
fn a_build_without_an_updater_reports_the_newer_release_only() {
    let server = Server::start();
    server.serve(
        "/latest",
        200,
        "application/json",
        format!(r#"{{"tag_name":"v{NEXT}"}}"#).into_bytes(),
    );
    let (_keep, staging) = staging_dir();
    let outcome = check(&server, &staging, &FakeSigner::signed_by(SIGNER), None).unwrap();
    assert!(matches!(outcome, Outcome::Newer { .. }));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
}

fn new_exe() -> Vec<u8> {
    pe(
        &[format!("new exe\0CA-BUILD-VERSION={NEXT}\0").as_bytes()],
        b"",
        None,
    )
}

fn new_ca() -> Vec<u8> {
    pe(
        &[format!("new ca\0CA-BUILD-VERSION={NEXT}\0").as_bytes()],
        b"",
        None,
    )
}

fn portable_zip() -> Vec<u8> {
    zip_bytes(&[
        ("compare-all.exe", &new_exe()),
        ("ca.exe", &new_ca()),
        ("README.md", b"new readme"),
    ])
}

fn stage_portable(server: &Server, staging: &Staging) {
    let zip = portable_zip();
    let asset = publish(server, &zip_name(NEXT), &zip, "application/zip");
    publish_manifest(server, vec![asset]);
    let outcome = check(
        server,
        staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Portable),
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Staged { .. }), "{outcome:?}");
}

fn program_folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("compare-all.exe"), b"old exe").unwrap();
    std::fs::write(dir.path().join("ca.exe"), b"old ca").unwrap();
    std::fs::write(dir.path().join("program-state.json"), b"{\"keep\":1}").unwrap();
    dir
}

fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn a_portable_update_replaces_program_files_and_keeps_settings() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let target = program_folder();
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    let version = apply_staged(
        &staging,
        InstallMode::Portable,
        &trust,
        &|_| panic!("no installer in portable mode"),
        &|_| Ok(()),
        target.path(),
    )
    .unwrap();
    assert_eq!(version.to_string(), NEXT);
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        new_exe()
    );
    assert_eq!(
        std::fs::read(target.path().join("ca.exe")).unwrap(),
        new_ca()
    );
    assert_eq!(
        std::fs::read(target.path().join("program-state.json")).unwrap(),
        b"{\"keep\":1}"
    );
    assert_eq!(
        names(target.path()),
        vec![
            "README.md",
            "ca.exe",
            "compare-all.exe",
            "program-state.json"
        ]
    );
}

#[test]
fn a_failed_portable_apply_restores_every_file() {
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(fresh.path().join("compare-all.exe"), b"new exe").unwrap();
    std::fs::write(fresh.path().join("ca.exe"), b"new ca").unwrap();
    std::fs::write(fresh.path().join("README.md"), b"new readme").unwrap();
    let target = program_folder();
    let before = names(target.path());
    // Files install in name order: README.md, ca.exe, then compare-all.exe.
    let result = apply_portable_with(target.path(), fresh.path(), &|name| {
        if name == "compare-all.exe" {
            Err(std::io::Error::other("disk full"))
        } else {
            Ok(())
        }
    });
    assert!(result.unwrap_err().contains("disk full"));
    assert_eq!(names(target.path()), before);
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"old exe"
    );
    assert_eq!(
        std::fs::read(target.path().join("ca.exe")).unwrap(),
        b"old ca"
    );
}

#[test]
fn a_changed_staged_file_is_refused_at_apply_time() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let record = staging.read_record().unwrap();
    let path = staging.asset_path(&record);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 30;
    bytes[last] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    let target = program_folder();
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    let result = apply_staged(
        &staging,
        InstallMode::Portable,
        &trust,
        &|_| Ok(()),
        &|_| Ok(()),
        target.path(),
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"old exe"
    );
}

#[test]
fn a_tampered_record_is_not_applied() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let record_path = staging.dir().join("staged.json");
    let text = std::fs::read_to_string(&record_path)
        .unwrap()
        .replace(NEXT, "2026.10.18.0099");
    std::fs::write(&record_path, text).unwrap();
    let target = program_folder();
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    let result = apply_staged(
        &staging,
        InstallMode::Portable,
        &trust,
        &|_| Ok(()),
        &|_| Ok(()),
        target.path(),
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"old exe"
    );
}

#[test]
fn an_archive_with_a_path_in_it_is_refused() {
    let server = Server::start();
    let zip = zip_bytes(&[("compare-all.exe", b"x"), ("..\\evil.exe", b"y")]);
    let asset = publish(&server, &zip_name(NEXT), &zip, "application/zip");
    publish_manifest(&server, vec![asset]);
    let (_keep, staging) = staging_dir();
    let outcome = check(
        &server,
        &staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Portable),
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::DownloadFailed { .. }));
}

/// Accepts a file only when its content holds `SIGNED`.
struct ContentSigner;

impl SignatureVerifier for ContentSigner {
    fn signer_facts(&self, file: &Path) -> Result<SignerFacts, String> {
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        if bytes.windows(6).any(|window| window == b"SIGNED") {
            Ok(SignerFacts::release(&format!("CN={SIGNER}, C=US")))
        } else {
            Err(format!("{} is not signed", file.display()))
        }
    }
}

fn signed_program(version: &str) -> Vec<u8> {
    pe(
        &[format!("SIGNED\0CA-BUILD-VERSION={version}\0").as_bytes()],
        b"",
        Some(b"signature"),
    )
}

fn marker(version: &str) -> Vec<u8> {
    format!("CA-BUILD-VERSION={version}\0").into_bytes()
}

#[test]
fn a_version_in_the_certificate_table_or_after_the_sections_is_refused() {
    let ca = signed_program(NEXT);
    let tag = marker(NEXT);
    let cases = [
        pe(&[b"SIGNED old build"], b"", Some(&tag)),
        pe(&[b"SIGNED old build"], &tag, None),
        pe(&[b"SIGNED old build"], &tag, Some(b"signature")),
    ];
    for (index, exe) in cases.iter().enumerate() {
        let server = Server::start();
        let (_keep, staging) = staging_dir();
        let outcome = stage_zip_with(
            &server,
            &staging,
            &[("compare-all.exe", exe), ("ca.exe", &ca)],
        )
        .unwrap();
        assert!(
            matches!(outcome, Outcome::DownloadFailed { .. }),
            "{index}: {outcome:?}"
        );
        assert!(staging.read_record().is_none(), "{index}");
    }
}

#[test]
fn every_program_of_the_archive_must_carry_the_release_version() {
    let exe = signed_program(NEXT);
    let tag = marker(NEXT);
    let cases = [
        signed_program("2026.09.30.0001"),
        pe(&[b"SIGNED old ca"], b"", Some(b"signature")),
        pe(&[b"SIGNED old ca"], b"", Some(&tag)),
    ];
    for (index, ca) in cases.iter().enumerate() {
        let server = Server::start();
        let (_keep, staging) = staging_dir();
        let outcome = stage_zip_with(
            &server,
            &staging,
            &[("compare-all.exe", &exe), ("ca.exe", ca)],
        )
        .unwrap();
        assert!(
            matches!(outcome, Outcome::DownloadFailed { .. }),
            "{index}: {outcome:?}"
        );
    }
}

fn stage_zip_with(
    server: &Server,
    staging: &Staging,
    files: &[(&str, &[u8])],
) -> Result<Outcome, String> {
    let zip = zip_bytes(files);
    let asset = publish(server, &zip_name(NEXT), &zip, "application/zip");
    publish_manifest(server, vec![asset]);
    let endpoints = server.endpoints();
    let cancel = Arc::new(AtomicBool::new(false));
    let transport = transport();
    run(&Job {
        transport: &transport,
        signer: &ContentSigner,
        staging,
        mode: Some(InstallMode::Portable),
        current: Version::parse(CURRENT).unwrap(),
        endpoints: &endpoints,
        signer_name: SIGNER,
        cancel: &cancel,
    })
}

#[test]
fn an_archive_entry_outside_the_release_file_set_is_refused() {
    let exe = signed_program(NEXT);
    let pe = b"MZ\x90\0not signed".to_vec();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("version.dll.", pe.clone()),
        ("options.json", b"{\"openWith\":[]}".to_vec()),
        ("NUL", b"x".to_vec()),
        ("extra.txt", b"x".to_vec()),
        ("COMPARE-ALL.EXE", exe.clone()),
        ("README.md", pe),
    ];
    for (name, data) in &cases {
        let server = Server::start();
        let (_keep, staging) = staging_dir();
        let outcome = stage_zip_with(
            &server,
            &staging,
            &[
                ("compare-all.exe", &exe),
                ("ca.exe", &exe),
                (name, data.as_slice()),
            ],
        )
        .unwrap();
        assert!(
            matches!(outcome, Outcome::DownloadFailed { .. }),
            "{name}: {outcome:?}"
        );
        assert!(staging.read_record().is_none(), "{name}");
    }
}

#[test]
fn an_older_archive_under_a_newer_version_is_not_applied() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    let exe = signed_program(NEXT);
    let outcome = stage_zip_with(
        &server,
        &staging,
        &[("compare-all.exe", &exe), ("ca.exe", &exe)],
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Staged { .. }), "{outcome:?}");
    let claimed = "2099.01.01.0001";
    let record = staging.read_record().unwrap();
    std::fs::rename(
        staging.asset_path(&record),
        staging.dir().join(zip_name(claimed)),
    )
    .unwrap();
    let text = std::fs::read_to_string(staging.dir().join("staged.json"))
        .unwrap()
        .replace(NEXT, claimed);
    std::fs::write(staging.dir().join("staged.json"), text).unwrap();
    assert!(staging.read_record().is_some());
    let target = program_folder();
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &ContentSigner,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    let result = apply_staged(
        &staging,
        InstallMode::Portable,
        &trust,
        &|_| Ok(()),
        &|_| Ok(()),
        target.path(),
    );
    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"old exe"
    );
}

/// Open `path` so that it cannot be renamed or deleted while the handle lives;
/// with `share_read` false it cannot be read either.
#[cfg(windows)]
fn pin(path: &Path, share_read: bool) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(u32::from(share_read))
        .open(path)
        .unwrap()
}

#[cfg(windows)]
#[test]
fn a_backup_that_cannot_be_moved_back_is_copied_back() {
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(fresh.path().join("compare-all.exe"), b"new exe").unwrap();
    std::fs::write(fresh.path().join("ca.exe"), b"new ca").unwrap();
    let target = program_folder();
    let pinned = Mutex::new(None);
    let result = apply_portable_with(target.path(), fresh.path(), &|name| {
        if name == "compare-all.exe" {
            let backup = backup_folder(target.path()).join("ca.exe");
            *pinned.lock().unwrap() = Some(pin(&backup, true));
            Err(std::io::Error::other("disk full"))
        } else {
            Ok(())
        }
    });
    let reason = result.unwrap_err();
    assert!(!reason.contains("restore"), "{reason}");
    assert_eq!(
        std::fs::read(target.path().join("ca.exe")).unwrap(),
        b"old ca"
    );
    drop(pinned);
}

fn backup_folder(target: &Path) -> std::path::PathBuf {
    std::fs::read_dir(target)
        .unwrap()
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".update-backup-")
        })
        .unwrap()
        .path()
}

#[cfg(windows)]
#[test]
fn a_failed_restore_leaves_the_new_program_file_in_place() {
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(fresh.path().join("compare-all.exe"), b"new exe").unwrap();
    std::fs::write(fresh.path().join("ca.exe"), b"new ca").unwrap();
    let target = program_folder();
    let pinned = Mutex::new(None);
    // ca.exe is replaced, then compare-all.exe fails while the backup of
    // ca.exe cannot be moved back.
    let result = apply_portable_with(target.path(), fresh.path(), &|name| {
        if name == "compare-all.exe" {
            let backup = backup_folder(target.path()).join("ca.exe");
            *pinned.lock().unwrap() = Some(pin(&backup, false));
            Err(std::io::Error::other("disk full"))
        } else {
            Ok(())
        }
    });
    let reason = result.unwrap_err();
    assert!(reason.contains("restore"), "{reason}");
    assert_eq!(
        std::fs::read(target.path().join("ca.exe")).unwrap(),
        b"new ca"
    );
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"old exe"
    );
    drop(pinned);
}

#[cfg(windows)]
#[test]
fn a_backup_that_cannot_be_removed_does_not_fail_the_update() {
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(fresh.path().join("compare-all.exe"), b"new exe").unwrap();
    std::fs::write(fresh.path().join("ca.exe"), b"new ca").unwrap();
    let target = program_folder();
    let pinned = Mutex::new(None);
    let result = apply_portable_with(target.path(), fresh.path(), &|name| {
        if name == "compare-all.exe" {
            let backup = backup_folder(target.path()).join("ca.exe");
            *pinned.lock().unwrap() = Some(pin(&backup, false));
        }
        Ok(())
    });
    assert_eq!(result, Ok(()));
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"new exe"
    );
    drop(pinned);
}

#[test]
fn a_program_file_that_cannot_be_opened_is_not_waited_for() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let target = program_folder();
    let arguments = ca_update::apply::HelperArguments {
        staging: staging.dir().to_path_buf(),
        executable: target.path().join("missing").join("compare-all.exe"),
        relaunch: false,
        program_arguments: Vec::new(),
    };
    let started = std::time::Instant::now();
    let code = ca_update::apply::helper_main_waiting(
        &arguments,
        &FakeSigner::signed_by(SIGNER),
        SIGNER,
        &running(),
        Duration::from_millis(500),
    );
    assert_ne!(code, 0);
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn a_failed_restore_keeps_the_backup_and_reports_it() {
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(fresh.path().join("compare-all.exe"), b"new exe").unwrap();
    std::fs::write(fresh.path().join("ca.exe"), b"new ca").unwrap();
    let target = program_folder();
    let live_ca = target.path().join("ca.exe");
    // ca.exe installs before compare-all.exe. The failure of compare-all.exe
    // leaves a folder where ca.exe must be restored, so that restore fails.
    let result = apply_portable_with(target.path(), fresh.path(), &|name| {
        if name == "compare-all.exe" {
            std::fs::remove_file(&live_ca)?;
            std::fs::create_dir(&live_ca)?;
            Err(std::io::Error::other("disk full"))
        } else {
            Ok(())
        }
    });
    let reason = result.unwrap_err();
    assert!(reason.contains("restore"), "{reason}");
    let backups: Vec<_> = std::fs::read_dir(target.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".update-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read(backups[0].path().join("ca.exe")).unwrap(),
        b"old ca"
    );
    assert!(reason.contains(&backups[0].file_name().to_string_lossy().into_owned()));
}

#[test]
fn a_second_apply_cannot_claim_the_same_staged_update() {
    let (_keep, staging) = staging_dir();
    let first = staging.begin_apply();
    assert!(first.is_ok());
    assert!(staging.begin_apply().is_err());
    drop(first);
}

#[test]
fn a_helper_that_finds_the_apply_claimed_leaves_everything_alone() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let target = program_folder();
    let _claim = staging.begin_apply();
    let arguments = ca_update::apply::HelperArguments {
        staging: staging.dir().to_path_buf(),
        executable: target.path().join("compare-all.exe"),
        relaunch: false,
        program_arguments: Vec::new(),
    };
    let code = ca_update::apply::helper_main_waiting(
        &arguments,
        &FakeSigner::signed_by(SIGNER),
        SIGNER,
        &running(),
        Duration::from_millis(500),
    );
    assert_ne!(code, 0);
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"old exe"
    );
    assert!(staging.read_record().is_some());
}

#[test]
fn a_helper_starts_the_program_only_after_it_ended_and_no_apply_runs_elsewhere() {
    let server = Server::start();
    let target = program_folder();
    let starts = AtomicUsize::new(0);
    let run = |staging: &Staging| {
        let arguments = ca_update::apply::HelperArguments {
            staging: staging.dir().to_path_buf(),
            executable: target.path().join("compare-all.exe"),
            relaunch: true,
            program_arguments: Vec::new(),
        };
        ca_update::apply::helper_main_with(
            &arguments,
            &FakeSigner::signed_by(SIGNER),
            SIGNER,
            &running(),
            Duration::from_millis(300),
            &|| {
                starts.fetch_add(1, Ordering::SeqCst);
            },
        )
    };
    // Another helper holds the claim.
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let claim = staging.begin_apply().unwrap();
    assert_ne!(run(&staging), 0);
    drop(claim);
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    // Another helper applied the update and cleared it. The program closed
    // with "Restart to update" comes back once it has ended.
    let (_keep, empty) = staging_dir();
    assert_ne!(run(&empty), 0);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    // This helper applies the update.
    assert_eq!(run(&staging), 0);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    // The end of the program is not seen.
    let (_keep, other) = staging_dir();
    stage_portable(&server, &other);
    let arguments = ca_update::apply::HelperArguments {
        staging: other.dir().to_path_buf(),
        executable: target.path().join("missing").join("compare-all.exe"),
        relaunch: true,
        program_arguments: Vec::new(),
    };
    let code = ca_update::apply::helper_main_with(
        &arguments,
        &FakeSigner::signed_by(SIGNER),
        SIGNER,
        &running(),
        Duration::from_millis(300),
        &|| {
            starts.fetch_add(1, Ordering::SeqCst);
        },
    );
    assert_ne!(code, 0);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    // Nothing staged and the end of the program is not seen.
    let (_keep, empty) = staging_dir();
    let arguments = ca_update::apply::HelperArguments {
        staging: empty.dir().to_path_buf(),
        ..arguments
    };
    let code = ca_update::apply::helper_main_with(
        &arguments,
        &FakeSigner::signed_by(SIGNER),
        SIGNER,
        &running(),
        Duration::from_millis(300),
        &|| {
            starts.fetch_add(1, Ordering::SeqCst);
        },
    );
    assert_ne!(code, 0);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
}

#[test]
fn a_program_file_changed_between_its_check_and_its_copy_is_not_installed() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let target = program_folder();
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    let swapped = b"MZ unverified program".to_vec();
    let staging_folder = staging.dir().to_path_buf();
    // Another process of the same user replaces the checked file in the
    // scratch folder after the check and before the copy.
    let swap = |_: &str| {
        for entry in std::fs::read_dir(&staging_folder).unwrap().flatten() {
            if entry.file_name().to_string_lossy().starts_with("scratch") {
                let _ = std::fs::write(entry.path().join("compare-all.exe"), &swapped);
            }
        }
        Ok(())
    };
    let _ = apply_staged(
        &staging,
        InstallMode::Portable,
        &trust,
        &|_| Ok(()),
        &swap,
        target.path(),
    );
    let installed = std::fs::read(target.path().join("compare-all.exe")).unwrap();
    assert_ne!(installed, swapped);
    assert!(installed == new_exe() || installed == b"old exe");
}

#[test]
fn a_check_while_another_copy_holds_the_claim_reports_it_and_changes_nothing() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let record = staging.read_record().unwrap();
    let before = std::fs::read(staging.asset_path(&record)).unwrap();
    let claim = staging.begin_apply().unwrap();
    let hits = server.hits.load(Ordering::SeqCst);
    let outcome = check(
        &server,
        &staging,
        &FakeSigner::signed_by(SIGNER),
        Some(InstallMode::Portable),
    )
    .unwrap();
    assert_eq!(outcome, Outcome::Busy);
    // The latest tag and the manifest only; no download.
    assert_eq!(server.hits.load(Ordering::SeqCst), hits + 2);
    assert_eq!(staging.read_record(), Some(record.clone()));
    assert_eq!(std::fs::read(staging.asset_path(&record)).unwrap(), before);
    drop(claim);
}

#[test]
fn the_machine_installer_command_writes_no_log_in_the_user_folder() {
    let args = |command: &std::process::Command| -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let program = Path::new(r"C:\Program Files\compare-all\compare-all.exe");
    let elevate = ca_update::apply::elevate_command(program, Path::new(r"C:\stage"));
    let installer =
        ca_update::install::installer_command(Path::new(r"C:\Program Files\work\a.msi"));
    for args in [args(&elevate), args(&installer)] {
        assert!(
            !args
                .iter()
                .any(|a| a.to_ascii_uppercase().starts_with("/L")),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a.contains(".log")), "{args:?}");
    }
}

#[cfg(windows)]
#[test]
fn the_elevation_prompt_starts_the_installed_program_and_not_the_installer() {
    let program = Path::new(r"C:\Program Files\compare-all\compare-all.exe");
    let command = ca_update::apply::elevate_command(program, Path::new(r"C:\stage"));
    assert!(command
        .get_program()
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(r"\system32\windowspowershell\v1.0\powershell.exe"));
    let script = command.get_args().last().unwrap().to_string_lossy();
    assert!(script.contains("Verb='runas'"), "{script}");
    assert!(
        script.contains(ca_update::install::INSTALL_ARGUMENT),
        "{script}"
    );
    assert!(!script.to_ascii_lowercase().contains("msiexec"), "{script}");
    let environment: HashMap<String, String> = command
        .get_envs()
        .filter_map(|(k, v)| {
            Some((
                k.to_string_lossy().into_owned(),
                v?.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    assert_eq!(
        environment.get("CA_PROGRAM").map(String::as_str),
        Some(program.to_str().unwrap())
    );
    assert_eq!(
        environment.get("CA_UPDATE_DIR").map(String::as_str),
        Some(r"C:\stage")
    );
    assert!(!environment.keys().any(|k| k.eq_ignore_ascii_case("TEMP")));
    assert!(!environment.keys().any(|k| k.eq_ignore_ascii_case("TMP")));

    // The elevated program runs the system installer on its own copy.
    let copy = Path::new(r"C:\Program Files\compare-all-update-7\a.msi");
    let installer = ca_update::install::installer_command(copy);
    assert!(installer
        .get_program()
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(r"\system32\msiexec.exe"));
    let args: Vec<String> = installer
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "/i",
            copy.to_str().unwrap(),
            "/passive",
            "/norestart",
            "ALLUSERS=1"
        ]
    );
    assert!(!installer
        .get_envs()
        .any(|(k, _)| k.eq_ignore_ascii_case("TEMP") || k.eq_ignore_ascii_case("TMP")));
}

/// A server that sends the headers at once and then the body in small pieces.
fn trickle_server(
    body: Vec<u8>,
    piece: usize,
    pause: Duration,
    stall_after: Option<usize>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let body = body.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                for (index, chunk) in body.chunks(piece).enumerate() {
                    if stall_after.is_some_and(|n| index >= n) {
                        std::thread::sleep(Duration::from_secs(30));
                        return;
                    }
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    let _ = stream.flush();
                    std::thread::sleep(pause);
                }
            });
        }
    });
    base
}

#[test]
fn a_slow_but_steady_download_outlasts_the_document_timeout() {
    use ca_update::net::Transport;
    let body = vec![7u8; 4000];
    let base = trickle_server(body.clone(), 200, Duration::from_millis(150), None);
    let transport = HttpTransport::new(
        "compare-all/test".to_owned(),
        ProxyConfig::direct(),
        Duration::from_secs(1),
    )
    .allowing_plain_http();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("file.part");
    let cancel = Arc::new(AtomicBool::new(false));
    let fetched = transport
        .download(&format!("{base}/f"), &out, body.len() as u64, &cancel)
        .unwrap();
    assert_eq!(fetched.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&out).unwrap(), body);
}

#[test]
fn a_stalled_download_ends_after_the_idle_limit() {
    use ca_update::net::Transport;
    let body = vec![7u8; 4000];
    let base = trickle_server(body.clone(), 200, Duration::from_millis(10), Some(3));
    let transport = HttpTransport::new(
        "compare-all/test".to_owned(),
        ProxyConfig::direct(),
        Duration::from_secs(1),
    )
    .allowing_plain_http();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("file.part");
    let cancel = Arc::new(AtomicBool::new(false));
    let started = std::time::Instant::now();
    let result = transport.download(&format!("{base}/f"), &out, body.len() as u64, &cancel);
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_trickling_download_ends_at_the_download_deadline() {
    use ca_update::net::Transport;
    // One byte each 100 ms never reaches the idle limit.
    let body = vec![7u8; 600];
    let base = trickle_server(body.clone(), 1, Duration::from_millis(100), None);
    let transport = HttpTransport::new(
        "compare-all/test".to_owned(),
        ProxyConfig::direct(),
        Duration::from_secs(1),
    )
    .with_download_deadline(Duration::from_secs(3))
    .allowing_plain_http();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("file.part");
    let cancel = Arc::new(AtomicBool::new(false));
    let started = std::time::Instant::now();
    let result = transport.download(&format!("{base}/f"), &out, 512 * 1024 * 1024, &cancel);
    let waited = started.elapsed();
    assert!(result.is_err(), "{result:?}");
    assert!(waited < Duration::from_secs(20), "{waited:?}");
}

#[test]
fn a_stalled_download_closes_its_connection_after_the_idle_limit() {
    use ca_update::net::Transport;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (closed_sender, closed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buffer = [0u8; 4096];
        let _ = stream.read(&mut buffer);
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 4000\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&[7u8; 600]);
        let _ = stream.flush();
        // Send nothing more and wait until the client closes the socket.
        let stalled = std::time::Instant::now();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(40)));
        let mut rest = [0u8; 16];
        let _ = stream.read(&mut rest);
        let _ = closed_sender.send(stalled.elapsed());
    });
    let transport = HttpTransport::new(
        "compare-all/test".to_owned(),
        ProxyConfig::direct(),
        Duration::from_secs(1),
    )
    .allowing_plain_http();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("file.part");
    let cancel = Arc::new(AtomicBool::new(false));
    // The size cap of a real release makes the outer limit days long.
    let result = transport.download(&format!("{base}/f"), &out, 512 * 1024 * 1024, &cancel);
    assert!(result.is_err());
    let waited = closed.recv_timeout(Duration::from_secs(60)).unwrap();
    assert!(waited < Duration::from_secs(10), "{waited:?}");
}

#[test]
fn the_installer_runs_elevated_only_for_an_install_for_every_user() {
    let server = Server::start();
    let msi = msi_bytes();
    let asset = publish(&server, &msi_name(NEXT), &msi, "application/x-msi");
    publish_manifest(&server, vec![asset]);
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    for machine in [false, true] {
        let (_keep, staging) = staging_dir();
        let mode = InstallMode::Installed { machine };
        assert!(matches!(
            check(&server, &staging, &signer, Some(mode)).unwrap(),
            Outcome::Staged { .. }
        ));
        let seen = Mutex::new(Vec::new());
        let target = program_folder();
        let result = apply_staged(
            &staging,
            mode,
            &trust,
            &|command| {
                seen.lock().unwrap().push(
                    command
                        .get_args()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect::<Vec<_>>(),
                );
                Ok(())
            },
            &|_| Ok(()),
            target.path(),
        );
        result.unwrap();
        let args = seen.lock().unwrap()[0].clone();
        if machine {
            // The installed program starts through an elevation prompt and
            // no log is written.
            let script = args.last().unwrap();
            assert!(script.contains("Verb='runas'"), "{script}");
            assert!(script.contains("--install-update"), "{script}");
            assert!(!args.iter().any(|a| a.contains("/L*v")), "{args:?}");
            continue;
        }
        assert_eq!(args[0], "/i");
        assert!(args.contains(&"/passive".to_owned()));
        assert!(!args.contains(&"ALLUSERS=1".to_owned()));
    }
}

fn stage_installer(server: &Server, mode: InstallMode) -> (tempfile::TempDir, Staging) {
    let msi = msi_bytes();
    let asset = publish(server, &msi_name(NEXT), &msi, "application/x-msi");
    publish_manifest(server, vec![asset]);
    let (keep, staging) = staging_dir();
    assert!(matches!(
        check(server, &staging, &FakeSigner::signed_by(SIGNER), Some(mode)).unwrap(),
        Outcome::Staged { .. }
    ));
    (keep, staging)
}

#[cfg(windows)]
#[test]
fn the_installer_file_cannot_be_replaced_while_the_installer_runs() {
    let server = Server::start();
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    for machine in [false, true] {
        let mode = InstallMode::Installed { machine };
        let (_keep, staging) = stage_installer(&server, mode);
        let msi = staging.asset_path(&staging.read_record().unwrap());
        let attempts = Mutex::new(Vec::new());
        let target = program_folder();
        let result = apply_staged(
            &staging,
            mode,
            &trust,
            &|_| {
                let mut seen = attempts.lock().unwrap();
                seen.push(std::fs::write(&msi, b"other installer").is_ok());
                seen.push(std::fs::rename(&msi, msi.with_extension("old")).is_ok());
                seen.push(std::fs::remove_file(&msi).is_ok());
                seen.push(std::fs::read(&msi).is_ok());
                Ok(())
            },
            &|_| Ok(()),
            target.path(),
        );
        assert!(result.is_ok(), "machine {machine}: {result:?}");
        assert_eq!(
            *attempts.lock().unwrap(),
            vec![false, false, false, true],
            "machine {machine}"
        );
        assert_eq!(std::fs::read(&msi).unwrap(), msi_bytes());
    }
}

/// Runs the real elevation script of an install for every user. The program
/// it elevates does not exist, so the start fails before any prompt, as a
/// declined or refused prompt does.
#[cfg(windows)]
#[test]
fn an_elevation_that_cannot_start_is_a_declined_elevation() {
    use ca_update::apply::{installer_result, ELEVATION_DECLINED};
    let dir = tempfile::tempdir().unwrap();
    let mut command =
        ca_update::apply::elevate_command(&dir.path().join("missing.exe"), dir.path());
    let status = command.status().unwrap();
    assert_eq!(
        installer_result(status.code()),
        Err(ELEVATION_DECLINED.to_owned()),
        "{status:?}"
    );
    // ERROR_FILE_NOT_FOUND from the inner Windows error, not a generic code.
    assert_eq!(
        ca_update::apply::elevation_error(status.code()),
        Some(2),
        "{status:?}"
    );
}

#[test]
fn every_elevation_that_did_not_happen_keeps_the_update() {
    use ca_update::apply::{installer_result, ELEVATION_DECLINED};
    let declined = Err(ELEVATION_DECLINED.to_owned());
    // Declined prompt, refused by policy, no error code, Cancel in the
    // installer window.
    for code in [0x2000_04C7, 0x2000_0005, 0x2000_0000, 1223, 1602] {
        assert_eq!(installer_result(Some(code)), declined, "{code:#x}");
    }
    // A crashed elevated program is a failure.
    #[allow(clippy::cast_possible_wrap)]
    let crashed = 0xC000_0005_u32 as i32;
    for code in [1, 2, 1603, crashed] {
        assert!(installer_result(Some(code)).is_err(), "{code:#x}");
        assert_ne!(installer_result(Some(code)), declined, "{code:#x}");
    }
    assert_ne!(installer_result(None), declined);
}

#[test]
fn a_declined_elevation_keeps_the_staged_update_without_a_warning() {
    use ca_update::apply::{finish_apply, installer_result, ELEVATION_DECLINED};
    assert_eq!(
        installer_result(Some(1223)),
        Err(ELEVATION_DECLINED.to_owned())
    );
    assert_eq!(installer_result(Some(0)), Ok(()));
    assert!(installer_result(Some(1603)).is_err());
    let server = Server::start();
    let mode = InstallMode::Installed { machine: true };
    let (_keep, staging) = stage_installer(&server, mode);
    let record = staging.read_record().unwrap();
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
        running: &running(),
    };
    let claim = staging.begin_apply().unwrap();
    let target = program_folder();
    let outcome = apply_staged(
        &staging,
        mode,
        &trust,
        &|_| installer_result(Some(1223)),
        &|_| Ok(()),
        target.path(),
    );
    assert_eq!(finish_apply(&staging, &claim, &outcome, &endpoints), 3);
    drop(claim);
    assert_eq!(staging.read_record(), Some(record.clone()));
    assert!(staging.asset_path(&record).exists());
    assert!(staging.read_warning().is_none());
    assert!(staging.read_apply_failure().is_none());

    // Any other failure removes the staged update and warns.
    let claim = staging.begin_apply().unwrap();
    let failed: Result<Version, String> = Err("the installer ended with code 1603".to_owned());
    assert_eq!(finish_apply(&staging, &claim, &failed, &endpoints), 1);
    drop(claim);
    assert!(staging.read_record().is_none());
    assert!(staging.read_warning().is_some());
}

/// `ERROR_INSTALL_ALREADY_RUNNING`: another install, such as a system
/// update, holds the installer. The update stays staged for the next close,
/// for an install for one user and for every user.
#[test]
fn an_installer_busy_with_another_install_keeps_the_staged_update() {
    use ca_update::apply::{finish_apply, installer_result};
    for machine in [false, true] {
        let server = Server::start();
        let mode = InstallMode::Installed { machine };
        let (_keep, staging) = stage_installer(&server, mode);
        let record = staging.read_record().unwrap();
        let signer = FakeSigner::signed_by(SIGNER);
        let endpoints = server.endpoints();
        let trust = Trust {
            signer: &signer,
            signer_name: SIGNER,
            endpoints: &endpoints,
            running: &running(),
        };
        let claim = staging.begin_apply().unwrap();
        let target = program_folder();
        let outcome = apply_staged(
            &staging,
            mode,
            &trust,
            &|_| installer_result(Some(1618)),
            &|_| Ok(()),
            target.path(),
        );
        assert!(outcome.is_err());
        assert_ne!(finish_apply(&staging, &claim, &outcome, &endpoints), 0);
        drop(claim);
        assert_eq!(staging.read_record(), Some(record.clone()), "{machine}");
        assert!(staging.asset_path(&record).exists());
        assert!(staging.read_warning().is_none());
        assert!(staging.read_apply_failure().is_none());
    }
}

/// An install that cannot apply updates by itself removes the staged update
/// once, warns, and does not download that release again; a newer release
/// is tried.
#[test]
fn a_release_this_install_cannot_apply_is_not_downloaded_again() {
    use ca_update::apply::{finish_apply, installer_result, CANNOT_APPLY_HERE};
    use ca_update::install::NOT_APPLICABLE;
    let server = Server::start();
    let mode = InstallMode::Installed { machine: true };
    let (_keep, staging) = stage_installer(&server, mode);
    let signer = FakeSigner::signed_by(SIGNER);
    let claim = staging.begin_apply().unwrap();
    let outcome = installer_result(Some(NOT_APPLICABLE)).map(|()| running());
    assert_eq!(outcome, Err(CANNOT_APPLY_HERE.to_owned()));
    assert_eq!(
        finish_apply(&staging, &claim, &outcome, &server.endpoints()),
        1
    );
    drop(claim);
    assert!(staging.read_record().is_none());
    assert!(staging.read_warning().is_some());

    let before = server.hits.load(Ordering::SeqCst);
    let outcome = check(&server, &staging, &signer, Some(mode)).unwrap();
    assert!(
        matches!(&outcome, Outcome::DownloadFailed { reason, .. } if reason == CANNOT_APPLY_HERE),
        "{outcome:?}"
    );
    // Only the latest release was read.
    assert_eq!(server.hits.load(Ordering::SeqCst) - before, 1);
    assert!(staging.read_record().is_none());
    assert!(staging.read_warning().is_some());
    assert_eq!(
        staging.read_apply_failure().as_deref(),
        Some(CANNOT_APPLY_HERE)
    );

    // Any other failure of the apply is tried again.
    staging.write_apply_failure("the installer ended with code 1603");
    assert!(matches!(
        check(&server, &staging, &signer, Some(mode)).unwrap(),
        Outcome::Staged { .. }
    ));
}

/// The install folder and program file of an install for every user must be
/// closed to other accounts; a folder in a profile is open to its user.
#[cfg(windows)]
#[test]
fn an_install_folder_open_to_other_accounts_is_refused() {
    use ca_update::install::{install_folder_closed, install_place_accepted, prepare_work_folder};
    let open = program_folder();
    let program = open.path().join("compare-all.exe");
    assert!(program.is_file());
    assert!(!install_folder_closed(&program));
    let (_keep, staging) = staging_dir();
    std::fs::create_dir_all(staging.dir()).unwrap();
    assert!(!install_place_accepted(staging.dir(), &program));

    // Administrators and the system only, as the program files folder gives.
    let parent = tempfile::tempdir().unwrap();
    let closed = prepare_work_folder(parent.path()).unwrap();
    let program = closed.path().join("compare-all.exe");
    std::fs::write(&program, b"old exe").unwrap();
    // The installer service owns the files it writes; a test process may
    // give new files the owner of its account.
    let status = ca_io::host_command::host_command("icacls")
        .arg(&program)
        .args(["/setowner", "*S-1-5-18", "/q"])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(install_folder_closed(&program));
    let system = ca_update::system::program_files().unwrap();
    assert!(install_folder_closed(&system.join("Common Files")));
    // A missing program file cannot be read and counts as open.
    assert!(!install_folder_closed(&closed.path().join("missing.exe")));
    let path = closed.path().to_path_buf();
    drop(closed);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn a_staged_version_not_newer_than_the_running_build_is_not_applied() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    stage_portable(&server, &staging);
    let target = program_folder();
    let signer = FakeSigner::signed_by(SIGNER);
    let endpoints = server.endpoints();
    for running in [NEXT, "2026.12.31.0001"] {
        let trust = Trust {
            signer: &signer,
            signer_name: SIGNER,
            endpoints: &endpoints,
            running: &Version::parse(running).unwrap(),
        };
        let result = apply_staged(
            &staging,
            InstallMode::Portable,
            &trust,
            &|_| Ok(()),
            &|_| Ok(()),
            target.path(),
        );
        assert!(result.is_err(), "{running}");
    }
    assert_eq!(
        std::fs::read(target.path().join("compare-all.exe")).unwrap(),
        b"old exe"
    );
}

fn elevated_setup() -> (
    Server,
    tempfile::TempDir,
    Staging,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let server = Server::start();
    let (keep, staging) = stage_installer(&server, InstallMode::Installed { machine: true });
    let work = tempfile::tempdir().unwrap();
    let install = tempfile::tempdir().unwrap();
    std::fs::write(install.path().join("compare-all.exe"), b"old exe").unwrap();
    (server, keep, staging, work, install)
}

fn folder_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

#[test]
fn the_elevated_step_installs_its_own_checked_copy_and_removes_it() {
    use ca_update::install::{install_from, Checks};
    let (_server, _keep, staging, work, install) = elevated_setup();
    let record = staging.read_record().unwrap();
    let program = install.path().join("compare-all.exe");
    let signer = FakeSigner::signed_by(SIGNER);
    let checks = Checks {
        signer: &signer,
        signer_name: SIGNER,
        running: &running(),
    };
    let staged_before = folder_files(staging.dir());
    let seen = Mutex::new(None);
    let code = install_from(&staging, work.path(), Some(&program), &checks, &|command| {
        let copy = std::path::PathBuf::from(command.get_args().nth(1).unwrap());
        let bytes = std::fs::read(&copy).unwrap();
        *seen.lock().unwrap() = Some((copy, bytes, std::fs::read(&program).ok()));
        Some(0)
    });
    assert_eq!(code, 0);
    let (copy, bytes, program_in_place) = seen.lock().unwrap().take().unwrap();
    assert_eq!(copy.parent(), Some(work.path()));
    assert_eq!(bytes, msi_bytes());
    // The install folder holds a program file at every moment.
    assert_eq!(program_in_place.as_deref(), Some(b"old exe".as_slice()));
    assert_eq!(names(install.path()), ["compare-all.exe"]);
    assert!(!copy.exists());
    // The elevated step writes nothing in the staging folder of the user.
    assert_eq!(folder_files(staging.dir()), staged_before);
    assert!(staging.asset_path(&record).exists());
}

#[test]
fn a_staged_installer_replaced_after_the_copy_is_not_what_is_installed() {
    use ca_update::install::{install_from, Checks};
    let (_server, _keep, staging, work, install) = elevated_setup();
    let staged = staging.asset_path(&staging.read_record().unwrap());
    let program = install.path().join("compare-all.exe");
    let signer = FakeSigner::signed_by(SIGNER);
    let checks = Checks {
        signer: &signer,
        signer_name: SIGNER,
        running: &running(),
    };
    let installed = Mutex::new(Vec::new());
    let code = install_from(&staging, work.path(), Some(&program), &checks, &|command| {
        // The file of the user is swapped between the check and the install.
        std::fs::write(&staged, b"other installer").unwrap();
        let copy = std::path::PathBuf::from(command.get_args().nth(1).unwrap());
        *installed.lock().unwrap() = std::fs::read(copy).unwrap();
        Some(0)
    });
    assert_eq!(code, 0);
    assert_eq!(*installed.lock().unwrap(), msi_bytes());
}

#[test]
fn a_failed_install_puts_the_program_file_back() {
    use ca_update::install::{install_from, Checks};
    let (_server, _keep, staging, work, install) = elevated_setup();
    let program = install.path().join("compare-all.exe");
    let signer = FakeSigner::signed_by(SIGNER);
    let checks = Checks {
        signer: &signer,
        signer_name: SIGNER,
        running: &running(),
    };
    let code = install_from(&staging, work.path(), Some(&program), &checks, &|_| {
        Some(1603)
    });
    assert_eq!(code, 1603);
    assert_eq!(std::fs::read(&program).unwrap(), b"old exe");
    assert_eq!(names(install.path()), ["compare-all.exe"]);
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
}

/// Start a copy of a system program that runs for a while as `program`.
#[cfg(windows)]
fn run_as(program: &Path) -> std::process::Child {
    let root = ca_update::system::system_root().unwrap();
    std::fs::copy(root.join(r"System32\PING.EXE"), program).unwrap();
    ca_io::host_command::host_command(program)
        .args(["-n", "120", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

#[cfg(windows)]
fn in_use(path: &Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(path).is_err()
}

/// The running program file is set aside before the installer runs. An
/// unused copy of it takes its place, so an interruption at any point
/// leaves a program file in the install folder that the installer can
/// replace.
#[cfg(windows)]
#[test]
fn the_install_folder_keeps_a_program_file_the_installer_can_replace() {
    use ca_update::install::{install_from, Checks};
    let signer = FakeSigner::signed_by(SIGNER);
    let checks = Checks {
        signer: &signer,
        signer_name: SIGNER,
        running: &running(),
    };
    for result in [1603, 0] {
        let (_server, _keep, staging, work, install) = elevated_setup();
        let program = install.path().join("compare-all.exe");
        std::fs::remove_file(&program).unwrap();
        let mut child = run_as(&program);
        let original = std::fs::read(&program).unwrap();
        assert!(in_use(&program));
        // (the program file has the original bytes, the file is in use)
        let state = || {
            (
                std::fs::read(&program).is_ok_and(|bytes| bytes == original),
                in_use(&program),
            )
        };
        let seen = Mutex::new(None);
        let code = install_from(&staging, work.path(), Some(&program), &checks, &|_| {
            *seen.lock().unwrap() = Some(state());
            Some(result)
        });
        let during = seen.lock().unwrap().take();
        let after = state();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(code, result);
        assert_eq!(during, Some((true, false)), "{result}");
        assert_eq!(names(install.path()), ["compare-all.exe"], "{result}");
        // After a success the installer has replaced the unused copy and the
        // running file is in the work folder; after a failure the running
        // file is back under its name.
        assert_eq!(after, (true, result != 0), "{result}");
    }
}

/// An interruption at any step of the set-aside leaves the complete program
/// file under its name.
#[cfg(windows)]
#[test]
fn setting_the_running_program_aside_never_leaves_its_name_empty() {
    let install = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let program = install.path().join("compare-all.exe");
    let mut child = run_as(&program);
    let original = std::fs::read(&program).unwrap();
    let seen = Mutex::new(Vec::new());
    let moved = ca_update::install::set_aside_observed(&program, work.path(), &|| {
        seen.lock()
            .unwrap()
            .push(std::fs::read(&program).is_ok_and(|bytes| bytes == original));
    });
    let after = std::fs::read(&program).is_ok_and(|bytes| bytes == original);
    let _ = child.kill();
    let _ = child.wait();
    assert!(moved.is_some());
    let seen = seen.into_inner().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|present| *present), "{seen:?}");
    assert!(after);
    assert!(!in_use(&program));
}

/// When the program file cannot be set aside, the installer does not start
/// and the staged update stays without a warning.
#[test]
fn an_install_whose_program_cannot_be_set_aside_does_not_start() {
    use ca_update::apply::{finish_apply, installer_result};
    use ca_update::install::{install_from, Checks, KEPT};
    let (server, _keep, staging, work, install) = elevated_setup();
    let program = install.path().join("compare-all.exe");
    // A folder under the name of the unused copy cannot be removed or
    // replaced by a file.
    let blocker = install.path().join("compare-all-standby.exe");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("held"), b"x").unwrap();
    let signer = FakeSigner::signed_by(SIGNER);
    let checks = Checks {
        signer: &signer,
        signer_name: SIGNER,
        running: &running(),
    };
    let started = AtomicBool::new(false);
    let code = install_from(&staging, work.path(), Some(&program), &checks, &|_| {
        started.store(true, Ordering::SeqCst);
        Some(0)
    });
    assert_eq!(code, KEPT);
    assert!(!started.load(Ordering::SeqCst));
    assert_eq!(std::fs::read(&program).unwrap(), b"old exe");
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);

    let record = staging.read_record().unwrap();
    let claim = staging.begin_apply().unwrap();
    let outcome = installer_result(Some(code)).map(|()| running());
    assert_eq!(
        finish_apply(&staging, &claim, &outcome, &server.endpoints()),
        4
    );
    drop(claim);
    let kept = staging.read_record().unwrap();
    assert!(kept.same_file(&record));
    assert_eq!(kept.put_off, 1);
    assert!(staging.read_warning().is_none());
    assert!(staging.read_apply_failure().is_none());
}

/// Any account that can read the program file can hold it without delete
/// sharing, so the elevated step keeps the update at each close. After a
/// fixed number of such closes the staged update is removed, the warning
/// with the release page shows, and that release is not downloaded again.
#[cfg(windows)]
#[test]
fn an_update_put_off_at_every_close_ends_in_the_warning() {
    use ca_update::apply::{finish_apply, installer_result, PUT_OFF_TOO_OFTEN};
    use ca_update::install::{install_from, Checks, KEPT};
    use ca_update::staging::PUT_OFF_LIMIT as LIMIT;
    let (server, _keep, staging, work, install) = elevated_setup();
    let mode = InstallMode::Installed { machine: true };
    let program = install.path().join("compare-all.exe");
    let signer = FakeSigner::signed_by(SIGNER);
    let checks = Checks {
        signer: &signer,
        signer_name: SIGNER,
        running: &running(),
    };
    let held = pin(&program, true);
    let code = install_from(&staging, work.path(), Some(&program), &checks, &|_| Some(0));
    drop(held);
    assert_eq!(code, KEPT);
    assert_eq!(std::fs::read(&program).unwrap(), b"old exe");

    let endpoints = server.endpoints();
    for close in 1..=LIMIT {
        let claim = staging.begin_apply().unwrap();
        let outcome = installer_result(Some(KEPT)).map(|()| running());
        let exit = finish_apply(&staging, &claim, &outcome, &endpoints);
        drop(claim);
        if close == LIMIT {
            assert_eq!(exit, 1);
            break;
        }
        assert_eq!(exit, 4, "close {close}");
        assert_eq!(staging.read_record().unwrap().put_off, close);
        assert!(staging.read_warning().is_none(), "close {close}");
        // A scheduled check between closes keeps the staged file and its count.
        let before = server.hits.load(Ordering::SeqCst);
        assert!(matches!(
            check(&server, &staging, &signer, Some(mode)).unwrap(),
            Outcome::Staged { .. }
        ));
        assert_eq!(server.hits.load(Ordering::SeqCst) - before, 2);
        assert_eq!(staging.read_record().unwrap().put_off, close);
    }
    assert!(staging.read_record().is_none());
    assert_eq!(staging.read_warning().unwrap().version, NEXT);
    let reason = staging.read_apply_failure().unwrap();
    assert_eq!(reason, PUT_OFF_TOO_OFTEN);

    // That release is not downloaded again.
    let before = server.hits.load(Ordering::SeqCst);
    let outcome = check(&server, &staging, &signer, Some(mode)).unwrap();
    assert!(
        matches!(&outcome, Outcome::DownloadFailed { reason: r, .. } if *r == reason),
        "{outcome:?}"
    );
    assert_eq!(server.hits.load(Ordering::SeqCst) - before, 1);
    assert!(staging.read_warning().is_some());

    // A release other than the one given up is staged with a new count.
    let other = Version::parse("2026.10.02.0001").unwrap();
    staging
        .write_warning(&ca_update::staging::DownloadWarning::new(
            &other,
            "https://example.test/other".to_owned(),
        ))
        .unwrap();
    assert!(matches!(
        check(&server, &staging, &signer, Some(mode)).unwrap(),
        Outcome::Staged { .. }
    ));
    assert_eq!(staging.read_record().unwrap().put_off, 0);
}

/// One elevated step at a time sets aside, installs and restores; a second
/// one waits a bounded time and then gives up without a change.
#[cfg(windows)]
#[test]
fn only_one_elevated_step_holds_the_install_folder() {
    use ca_update::install::{hold_install_lock, INSTALL_LOCK_NAME};
    let parent = tempfile::tempdir().unwrap();
    let first = hold_install_lock(parent.path(), Duration::ZERO).unwrap();
    let started = std::time::Instant::now();
    assert!(hold_install_lock(parent.path(), Duration::from_millis(600)).is_none());
    assert!(started.elapsed() >= Duration::from_millis(600));
    // Another account cannot delete or replace the held file either.
    assert!(std::fs::remove_file(parent.path().join(INSTALL_LOCK_NAME)).is_err());
    drop(first);
    assert!(!parent.path().join(INSTALL_LOCK_NAME).exists());
    let second = hold_install_lock(parent.path(), Duration::ZERO);
    assert!(second.is_some());
    drop(second);
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
}

fn flip_a_byte(staging: &Staging) {
    let path = staging.asset_path(&staging.read_record().unwrap());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[20] ^= 1;
    std::fs::write(path, bytes).unwrap();
}

fn add_a_byte(staging: &Staging) {
    let path = staging.asset_path(&staging.read_record().unwrap());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(0);
    std::fs::write(path, bytes).unwrap();
}

fn break_the_record(staging: &Staging) {
    std::fs::write(staging.dir().join("staged.json"), b"{").unwrap();
}

fn leave_alone(_: &Staging) {}

/// Stage `bytes` with a record of its own for version `version`, as a
/// process of the same user can: the record matches the file.
fn restage(staging: &Staging, version: &str, bytes: &[u8]) {
    let version = Version::parse(version).unwrap();
    let asset = msi_name(&version.to_string());
    std::fs::write(staging.dir().join(&asset), bytes).unwrap();
    let record = ca_update::staging::StagedRecord::new(
        &version,
        asset,
        sha(bytes),
        u64::try_from(bytes.len()).unwrap(),
    );
    staging.write_record(&record).unwrap();
}

/// A signed installer of another product under a record for a far newer
/// release.
fn other_product(staging: &Staging) {
    let bytes = msi_package("{00000000-1111-2222-3333-444444444444}", "99.1.1");
    restage(staging, "2099.01.01.0001", &bytes);
}

/// The installed release, or an older one, under a record for a newer one.
fn older_package(staging: &Staging) {
    restage(
        staging,
        "2099.01.01.0001",
        &msi_package(UPGRADE_CODE, "26.10.1"),
    );
}

/// The installer of a newer release under a record of another version.
fn record_of_another_version(staging: &Staging) {
    restage(staging, "2026.11.01.0009", &msi_bytes());
}

/// A file that is no installer database.
fn not_a_database(staging: &Staging) {
    let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    bytes.extend(std::iter::repeat_n(7u8, 300));
    restage(staging, NEXT, &bytes);
}

/// A record that names more bytes than any release may have; the elevated
/// step copies and parses nothing.
fn over_the_size_cap(staging: &Staging) {
    let mut record = staging.read_record().unwrap();
    record.size = ca_update::staging::MAX_ASSET_BYTES + 1;
    staging.write_record(&record).unwrap();
}

/// Name, change to the staged files, verifier, installed version.
type Case<'a> = (&'a str, fn(&Staging), &'a FakeSigner, &'a Version);

#[test]
fn the_elevated_step_refuses_a_staged_installer_that_fails_a_check() {
    use ca_update::install::{install_from, Checks, REFUSED};
    let signer = FakeSigner::signed_by(SIGNER);
    let other = FakeSigner::signed_by("Someone Else");
    let newer = Version::parse("2026.12.31.0001").unwrap();
    let current = running();
    let cases: [Case<'_>; 10] = [
        ("changed", flip_a_byte, &signer, &current),
        ("longer", add_a_byte, &signer, &current),
        ("bad record", break_the_record, &signer, &current),
        ("other signer", leave_alone, &other, &current),
        ("not newer", leave_alone, &signer, &newer),
        ("other product", other_product, &signer, &current),
        ("older package", older_package, &signer, &current),
        (
            "other version",
            record_of_another_version,
            &signer,
            &current,
        ),
        ("no database", not_a_database, &signer, &current),
        ("over the size cap", over_the_size_cap, &signer, &current),
    ];
    for (name, tamper, verifier, installed) in cases {
        let (_server, _keep, staging, work, install) = elevated_setup();
        let program = install.path().join("compare-all.exe");
        tamper(&staging);
        let before = folder_files(staging.dir());
        let checks = Checks {
            signer: verifier,
            signer_name: SIGNER,
            running: installed,
        };
        let ran = AtomicBool::new(false);
        let code = install_from(&staging, work.path(), Some(&program), &checks, &|_| {
            ran.store(true, Ordering::SeqCst);
            Some(0)
        });
        assert_eq!(code, REFUSED, "{name}");
        assert!(!ran.load(Ordering::SeqCst), "{name}");
        assert_eq!(std::fs::read(&program).unwrap(), b"old exe", "{name}");
        assert_eq!(folder_files(staging.dir()), before, "{name}");
        assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0, "{name}");
    }
}

#[test]
fn the_elevated_program_needs_its_own_argument_and_one_folder() {
    use ca_update::install::{parse_install_arguments, INSTALL_ARGUMENT};
    use std::ffi::OsString;
    let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    assert_eq!(
        parse_install_arguments(&args(&[INSTALL_ARGUMENT, r"C:\stage"])),
        Some(std::path::PathBuf::from(r"C:\stage"))
    );
    assert_eq!(parse_install_arguments(&args(&[INSTALL_ARGUMENT])), None);
    assert_eq!(
        parse_install_arguments(&args(&[INSTALL_ARGUMENT, "a", "b"])),
        None
    );
    assert_eq!(
        parse_install_arguments(&args(&["--apply-update", "a"])),
        None
    );
}

/// A junction at `link` to `target`. Junctions need no privilege.
#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let status = ca_io::host_command::host_command("cmd")
        .arg("/d")
        .arg("/c")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(windows)]
#[test]
fn the_elevated_step_reads_only_a_local_staging_folder_inside_a_profile() {
    use ca_update::install::accepted_staging;
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let root_text = root_path.to_string_lossy();
    let root_plain = Path::new(root_text.trim_start_matches(r"\\?\")).to_path_buf();
    let profile = root_plain.join("profile");
    let staging = profile.join(r"AppData\Local\compare-all\updates");
    std::fs::create_dir_all(&staging).unwrap();
    let elsewhere = root_plain.join(r"elsewhere\compare-all\updates");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let wrong_tail = profile.join(r"AppData\Local\other\updates");
    std::fs::create_dir_all(&wrong_tail).unwrap();
    let profiles = [profile.clone()];

    assert_eq!(accepted_staging(&staging, &profiles), Some(staging.clone()));
    let upper = std::path::PathBuf::from(staging.to_string_lossy().to_uppercase());
    assert!(accepted_staging(&upper, &profiles).is_some());

    let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", staging.display()));
    let device = std::path::PathBuf::from(format!(r"\\.\{}", staging.display()));
    let drive = staging.to_string_lossy()[..1].to_owned();
    let unc = std::path::PathBuf::from(format!(
        r"\\localhost\{drive}${}",
        &staging.to_string_lossy()[2..]
    ));
    let dotted = profile.join(r"AppData\Local\compare-all\..\compare-all\updates");
    let refused = [
        ("network", unc),
        ("verbatim", verbatim),
        ("device", device),
        ("relative", std::path::PathBuf::from(r"compare-all\updates")),
        ("dotted", dotted),
        ("outside", elsewhere.clone()),
        ("tail", wrong_tail),
        ("missing", profile.join(r"x\compare-all\updates")),
    ];
    for (name, path) in refused {
        assert_eq!(accepted_staging(&path, &profiles), None, "{name}");
    }
    assert_eq!(accepted_staging(&staging, &[]), None);

    // A link inside the profile that leads out of it.
    let linked = profile.join(r"AppData\Roaming\compare-all");
    std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
    junction(&linked, elsewhere.parent().unwrap());
    assert!(linked.join("updates").is_dir());
    assert_eq!(accepted_staging(&linked.join("updates"), &profiles), None);
}

#[test]
fn profile_folders_expand_only_the_system_folders() {
    use ca_update::system::expand_system_folders;
    let root = Path::new(r"C:\Windows");
    assert_eq!(
        expand_system_folders(r"%SystemDrive%\Users\a", Some(root)),
        Some(std::path::PathBuf::from(r"C:\Users\a"))
    );
    assert_eq!(
        expand_system_folders(r"%systemroot%\System32\config\systemprofile", Some(root)),
        Some(std::path::PathBuf::from(
            r"C:\Windows\System32\config\systemprofile"
        ))
    );
    assert_eq!(expand_system_folders(r"%USERPROFILE%\x", Some(root)), None);
    assert_eq!(expand_system_folders(r"%SystemDrive%\x", None), None);
}

/// Replace the staged installer, or with `record` the staged record, by a
/// symbolic link to a copy of it. `false` when this account may not create
/// symbolic links.
#[cfg(windows)]
fn link_staged(staging: &Staging, record: bool) -> bool {
    let name = if record {
        "staged.json".to_owned()
    } else {
        staging.read_record().unwrap().asset
    };
    let path = staging.dir().join(&name);
    let real = staging.dir().join(format!("real-{name}"));
    std::fs::rename(&path, &real).unwrap();
    if std::os::windows::fs::symlink_file(&real, &path).is_err() {
        std::fs::rename(&real, &path).unwrap();
        return false;
    }
    true
}

#[cfg(windows)]
#[test]
fn the_elevated_step_reads_no_staged_file_through_a_link() {
    use ca_update::install::{install_from, Checks, REFUSED};
    for record in [false, true] {
        let (_server, _keep, staging, work, install) = elevated_setup();
        if !link_staged(&staging, record) {
            eprintln!("symbolic links need a privilege this account lacks");
            return;
        }
        let program = install.path().join("compare-all.exe");
        let signer = FakeSigner::signed_by(SIGNER);
        let checks = Checks {
            signer: &signer,
            signer_name: SIGNER,
            running: &running(),
        };
        let code = install_from(&staging, work.path(), Some(&program), &checks, &|_| Some(0));
        assert_eq!(code, REFUSED, "record: {record}");
    }
}

/// The helper copy is held from its write until the helper runs from it.
#[cfg(windows)]
#[test]
fn the_helper_copy_cannot_be_replaced_before_it_starts() {
    let root = ca_update::system::system_root().unwrap();
    let source = root.join("System32").join("where.exe");
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("helper.exe");
    std::fs::write(&copy, b"left from an earlier run").unwrap();
    let held = ca_update::verify::copy_held(&source, &copy).unwrap();
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(&source).unwrap()
    );
    assert!(std::fs::write(&copy, b"other program").is_err());
    assert!(std::fs::rename(&copy, dir.path().join("moved.exe")).is_err());
    assert!(std::fs::remove_file(&copy).is_err());
    // The system starts a program from the held copy.
    let status = ca_io::host_command::host_command(&copy)
        .arg("/?")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.code().is_some());
    drop(held);
}

/// The access list of `folder` in the system's text form.
#[cfg(windows)]
fn access_list(folder: &Path) -> String {
    let saved = tempfile::tempdir().unwrap();
    let file = saved.path().join("acl.txt");
    let status = ca_io::host_command::host_command("icacls")
        .arg(folder)
        .arg("/save")
        .arg(&file)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&file).unwrap();
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(windows)]
#[test]
fn the_work_folder_is_open_to_administrators_and_the_system_only() {
    use ca_update::install::{prepare_work_folder, sweep_work_folders, WORK_FOLDER_PREFIX};
    let parent = tempfile::tempdir().unwrap();
    let left = parent.path().join(format!("{WORK_FOLDER_PREFIX}1"));
    std::fs::create_dir(&left).unwrap();
    std::fs::write(left.join("old.msi"), b"old").unwrap();
    let other = parent.path().join("other");
    std::fs::create_dir(&other).unwrap();
    sweep_work_folders(parent.path());
    assert!(!left.exists());
    assert!(other.exists());

    let held = prepare_work_folder(parent.path()).unwrap();
    let work = held.path().to_path_buf();
    assert!(work
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(&format!("{WORK_FOLDER_PREFIX}{}-", std::process::id())));
    // Owned by Administrators whatever owner the token gives new objects.
    assert_eq!(owner_of(&work), "O:BA");
    let acl = access_list(&work);
    let dacl = acl
        .split("D:")
        .nth(1)
        .unwrap_or_default()
        .trim_end_matches(['\r', '\n', '\0', ' ']);
    // Protected, with no inherited entry: full control for Administrators
    // (BA) and SYSTEM (SY), for the folder, its files and its subfolders.
    assert!(dacl.starts_with("PAI("), "{acl}");
    let mut entries: Vec<&str> = dacl["PAI".len()..].split_inclusive(')').collect();
    entries.sort_unstable();
    assert_eq!(entries, ["(A;OICI;FA;;;BA)", "(A;OICI;FA;;;SY)"], "{acl}");
    // The owner can still reset the list, which the clean-up needs.
    let status = ca_io::host_command::host_command("icacls")
        .arg(&work)
        .arg("/reset")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    drop(held);
    std::fs::remove_dir_all(&work).unwrap();
}

/// The owner of `path` in the system's text form.
#[cfg(windows)]
fn owner_of(path: &Path) -> String {
    let root = ca_update::system::system_root().unwrap();
    let output = ca_io::host_command::host_command(
        root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    )
    .args(["-NoProfile", "-NonInteractive", "-Command"])
    .arg("(Get-Acl -LiteralPath $env:CA_TARGET).GetSecurityDescriptorSddlForm('Owner')")
    .env("CA_TARGET", path)
    // A module path inherited from a newer PowerShell loads modules that
    // Windows PowerShell cannot run, and Get-Acl then fails.
    .env(
        "PSModulePath",
        root.join(r"System32\WindowsPowerShell\v1.0\Modules"),
    )
    .output()
    .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[cfg(windows)]
#[test]
fn a_sweep_removes_only_the_work_folders_of_ended_steps() {
    use ca_update::install::{prepare_work_folder, sweep_work_folders, WORK_FOLDER_PREFIX};
    let parent = tempfile::tempdir().unwrap();
    let first = prepare_work_folder(parent.path()).unwrap();
    let second = prepare_work_folder(parent.path()).unwrap();
    assert_ne!(first.path(), second.path());
    std::fs::write(first.path().join("copy.msi"), b"in use").unwrap();
    std::fs::write(second.path().join("copy.msi"), b"in use").unwrap();
    // A folder without a lock: left by a step that ended before it held one.
    let stale = parent.path().join(format!("{WORK_FOLDER_PREFIX}1"));
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("old.msi"), b"old").unwrap();
    sweep_work_folders(parent.path());
    assert!(first.path().join("copy.msi").exists());
    assert!(second.path().join("copy.msi").exists());
    assert!(!stale.exists());
    let ended = first.path().to_path_buf();
    drop(first);
    sweep_work_folders(parent.path());
    assert!(!ended.exists());
    assert!(second.path().join("copy.msi").exists());
}
