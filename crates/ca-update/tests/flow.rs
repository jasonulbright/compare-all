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

fn msi_bytes() -> Vec<u8> {
    let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    bytes.extend(std::iter::repeat_n(7u8, 300));
    bytes
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
    let command = ca_update::apply::msi_command(
        Path::new(r"C:\stage\a.msi"),
        true,
        Path::new(r"C:\stage\installer.log"),
    );
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        !args.iter().any(|a| a.eq_ignore_ascii_case("/L*v")),
        "{args:?}"
    );
    assert!(
        !args.iter().any(|a| a.contains("installer.log")),
        "{args:?}"
    );
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
            // The installer starts through an elevation prompt and writes no log.
            let script = args.last().unwrap();
            assert!(script.contains("-Verb RunAs"), "{script}");
            assert!(script.contains("ALLUSERS=1"), "{script}");
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
