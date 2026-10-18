//! The check, download, verification and apply steps against a local server.
//! Nothing here reaches the internet.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_update::apply::{apply_portable_with, apply_staged, Trust};
use ca_update::flow::{run, Endpoints, Job, Outcome};
use ca_update::manifest::{msi_name, zip_name, Manifest, ManifestAsset, MANIFEST_NAME, SCHEMA};
use ca_update::mode::InstallMode;
use ca_update::net::{HttpTransport, ProxyConfig};
use ca_update::signature::SignatureVerifier;
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
    fn signer_subject(&self, _: &Path) -> Result<String, String> {
        self.0.lock().unwrap().clone()
    }
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

fn portable_zip() -> Vec<u8> {
    zip_bytes(&[
        ("compare-all.exe", b"new exe"),
        ("ca.exe", b"new ca"),
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
        b"new exe"
    );
    assert_eq!(
        std::fs::read(target.path().join("ca.exe")).unwrap(),
        b"new ca"
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

#[test]
fn the_installer_runs_for_an_installed_copy_with_the_machine_scope() {
    let server = Server::start();
    let (_keep, staging) = staging_dir();
    let msi = msi_bytes();
    let asset = publish(&server, &msi_name(NEXT), &msi, "application/x-msi");
    publish_manifest(&server, vec![asset]);
    let mode = InstallMode::Installed { machine: true };
    let signer = FakeSigner::signed_by(SIGNER);
    assert!(matches!(
        check(&server, &staging, &signer, Some(mode)).unwrap(),
        Outcome::Staged { .. }
    ));
    let endpoints = server.endpoints();
    let trust = Trust {
        signer: &signer,
        signer_name: SIGNER,
        endpoints: &endpoints,
    };
    let seen = Mutex::new(Vec::new());
    let target = program_folder();
    apply_staged(
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
    )
    .unwrap();
    let args = seen.lock().unwrap()[0].clone();
    assert_eq!(args[0], "/i");
    assert!(args.contains(&"/passive".to_owned()));
    assert!(args.contains(&"ALLUSERS=1".to_owned()));
}
