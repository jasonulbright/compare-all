//! Fetching the pack list, installing a pack from a zip and removing it.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

mod pack_net;

use ca_ui::pack::index::{IndexEntry, MARKER_FILE};
use ca_ui::pack::remote::{
    self, builtin_ids, FetchError, IndexFailure, InstallStage, PackServices,
};
use pack_net::{
    entry_for, good_zip, listing, locations, manifest, png, serve_index, serve_pack, services,
    zip_of, FakeTransport,
};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

struct Setup {
    _dir: tempfile::TempDir,
    packs: std::path::PathBuf,
    transport: Arc<FakeTransport>,
    services: Arc<PackServices>,
    cancel: Arc<AtomicBool>,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let packs = dir.path().join("packs");
    let transport = Arc::new(FakeTransport::default());
    let services = services(&transport, &packs);
    Setup {
        _dir: dir,
        packs,
        transport,
        services,
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

fn install(setup: &Setup, entry: &IndexEntry) -> Result<(), remote::InstallError> {
    remote::install(&setup.services, entry, &setup.cancel)
}

/// Install `zip` as pack `neon` and return the result and the folder listing.
fn install_zip(zip: Vec<u8>) -> (Result<(), remote::InstallError>, Vec<String>, Setup) {
    let setup = setup();
    let entry = serve_pack(&setup.transport, "neon", "Neon", zip);
    let result = install(&setup, &entry);
    let names = listing(&setup.packs);
    (result, names, setup)
}

#[test]
fn a_good_pack_installs_with_a_marker_and_leaves_no_temporary_folder() {
    let (result, names, setup) = install_zip(good_zip("neon", "Neon"));
    result.unwrap();
    assert_eq!(names, ["neon"]);
    let folder = setup.packs.join("neon");
    assert!(folder.join("pack.json").is_file());
    assert!(folder.join("icons/16/next-difference.png").is_file());
    let entry = entry_for("neon", "Neon", &good_zip("neon", "Neon"), &png(32));
    assert_eq!(
        std::fs::read_to_string(folder.join(MARKER_FILE)).unwrap(),
        entry.sha256
    );
    assert_eq!(remote::installed_by_browser(&setup.packs).len(), 1);
}

#[test]
fn an_installed_pack_is_replaced_by_a_new_install() {
    let setup = setup();
    let first = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    install(&setup, &first).unwrap();
    let newer = zip_of(&[
        ("pack.json", manifest("neon", "Neon Two")),
        ("icons/16/next-difference.png", png(16)),
        ("extra.txt", b"new".to_vec()),
    ]);
    let second = serve_pack(&setup.transport, "neon", "Neon Two", newer);
    install(&setup, &second).unwrap();
    assert_eq!(listing(&setup.packs), ["neon"]);
    assert!(setup.packs.join("neon/extra.txt").is_file());
    assert_eq!(
        remote::installed_by_browser(&setup.packs)["neon"],
        second.sha256
    );
}

#[test]
fn unsafe_entry_names_are_refused_and_leave_nothing() {
    for name in [
        "../x",
        "icons/../../x",
        "/abs",
        "C:/x",
        "C:x",
        "icons\\a.png",
        "\\\\server\\share\\x",
        "CON",
        "con.txt",
        "icons/nul.png",
        "AUX",
        "COM1.png",
        "Icons/A.png",
        ".downloaded",
        "a//b",
    ] {
        let zip = zip_of(&[
            ("pack.json", manifest("neon", "Neon")),
            (name, b"data".to_vec()),
        ]);
        let (result, names, setup) = install_zip(zip);
        let error = result
            .err()
            .unwrap_or_else(|| panic!("{name} was installed"));
        assert_eq!(error.stage, InstallStage::Refused, "{name}");
        assert!(
            error.message.contains("not a plain relative path"),
            "{name}: {}",
            error.message
        );
        assert!(names.is_empty(), "{name}: {names:?}");
        assert!(
            !setup.packs.parent().unwrap().join("x").exists(),
            "{name} escaped"
        );
    }
}

#[test]
fn a_name_of_129_characters_is_refused() {
    let long = format!("{}.png", "a".repeat(125));
    assert_eq!(long.len(), 129);
    let zip = zip_of(&[("pack.json", manifest("neon", "Neon")), (&long, vec![1])]);
    let (result, names, _setup) = install_zip(zip);
    assert!(result.is_err());
    assert!(names.is_empty());
}

#[test]
fn a_directory_entry_is_refused() {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.add_directory("icons/", options).unwrap();
        writer.finish().unwrap();
    }
    let (result, names, _setup) = install_zip(cursor.into_inner());
    assert!(result.unwrap_err().message.contains("regular file"));
    assert!(names.is_empty());
}

#[test]
fn a_file_over_the_file_limit_is_refused() {
    let big = vec![7u8; 128 * 1024 + 1];
    let zip = zip_of(&[("pack.json", manifest("neon", "Neon")), ("big.bin", big)]);
    let (result, names, _setup) = install_zip(zip);
    assert!(result.unwrap_err().message.contains("larger than"));
    assert!(names.is_empty());
}

#[test]
fn a_pack_over_the_total_limit_is_refused() {
    let chunk = vec![0u8; 120 * 1024];
    let mut entries: Vec<(String, Vec<u8>)> =
        vec![("pack.json".to_owned(), manifest("neon", "Neon"))];
    for number in 0..140 {
        entries.push((format!("f{number}.bin"), chunk.clone()));
    }
    let borrowed: Vec<(&str, Vec<u8>)> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.clone()))
        .collect();
    let (result, names, _setup) = install_zip(zip_of(&borrowed));
    assert!(result.unwrap_err().message.contains("unpacked"));
    assert!(names.is_empty());
}

#[test]
fn a_zip_with_too_many_entries_is_refused() {
    let mut entries: Vec<(String, Vec<u8>)> =
        vec![("pack.json".to_owned(), manifest("neon", "Neon"))];
    for number in 0..1024 {
        entries.push((format!("f{number}.txt"), Vec::new()));
    }
    let borrowed: Vec<(&str, Vec<u8>)> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.clone()))
        .collect();
    let (result, names, _setup) = install_zip(zip_of(&borrowed));
    assert!(result.unwrap_err().message.contains("more than 1024"));
    assert!(names.is_empty());
}

#[test]
fn a_changed_zip_fails_the_checksum_and_the_byte_count() {
    let setup = setup();
    let zip = good_zip("neon", "Neon");
    let mut entry = serve_pack(&setup.transport, "neon", "Neon", zip.clone());
    entry.sha256 = "0".repeat(64);
    let error = install(&setup, &entry).unwrap_err();
    assert_eq!(error.stage, InstallStage::Refused);
    assert!(error.message.contains("does not match the pack list"));

    let mut shorter = serve_pack(&setup.transport, "neon", "Neon", zip.clone());
    shorter.size += 1;
    let error = install(&setup, &shorter).unwrap_err();
    assert!(error.message.contains("does not match the pack list"));
    assert!(listing(&setup.packs).is_empty());
}

#[test]
fn a_download_over_the_listed_size_is_a_download_failure() {
    let setup = setup();
    let mut entry = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    entry.size -= 1;
    let error = install(&setup, &entry).unwrap_err();
    assert_eq!(error.stage, InstallStage::Download);
    assert!(listing(&setup.packs).is_empty());
}

#[test]
fn a_failed_transfer_is_a_download_failure() {
    let setup = setup();
    let entry = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    setup.transport.fail(
        &locations().file_url(&entry.asset),
        FetchError::Failed("blocked by the proxy".to_owned()),
    );
    let error = install(&setup, &entry).unwrap_err();
    assert_eq!(error.stage, InstallStage::Download);
    assert!(error.message.contains("blocked by the proxy"));
    assert!(listing(&setup.packs).is_empty());
}

#[test]
fn a_manifest_id_other_than_the_listed_id_is_refused() {
    let (result, names, _setup) = install_zip(good_zip("other", "Other"));
    let error = result.unwrap_err();
    assert!(error.message.contains("another identifier"));
    assert!(names.is_empty());
}

#[test]
fn the_loader_limits_apply_to_a_downloaded_pack() {
    let zip = zip_of(&[
        ("pack.json", manifest("neon", "Neon")),
        ("icons/16/next-difference.png", png(24)),
    ]);
    let (result, names, _setup) = install_zip(zip);
    assert!(result.unwrap_err().message.contains("must be 16 by 16"));
    assert!(names.is_empty());

    let (result, names, _setup) = install_zip(zip_of(&[("icons/x.png", png(16))]));
    assert!(result.unwrap_err().message.contains("pack.json"));
    assert!(names.is_empty());

    let (result, names, _setup) = install_zip(b"not a zip".to_vec());
    assert!(result.unwrap_err().message.contains("not a usable zip"));
    assert!(names.is_empty());
}

#[test]
fn a_pack_copied_by_hand_is_not_replaced_and_not_removed() {
    let setup = setup();
    let folder = setup.packs.join("neon");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("pack.json"), manifest("neon", "Mine")).unwrap();
    let entry = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    let error = install(&setup, &entry).unwrap_err();
    assert!(error.message.contains("was not changed") || error.message.contains("not changed"));
    assert_eq!(
        std::fs::read_to_string(folder.join("pack.json")).unwrap(),
        String::from_utf8(manifest("neon", "Mine")).unwrap()
    );
    assert_eq!(listing(&setup.packs), ["neon"]);
    assert!(remote::remove(&setup.packs, "neon").is_err());
    assert!(folder.join("pack.json").is_file());
}

#[test]
fn a_marker_that_is_not_a_plain_checksum_does_not_make_a_pack_removable() {
    let setup = setup();
    let folder = setup.packs.join("neon");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("pack.json"), manifest("neon", "Mine")).unwrap();
    std::fs::write(folder.join(MARKER_FILE), "yes").unwrap();
    assert!(remote::remove(&setup.packs, "neon").is_err());
    assert!(folder.is_dir());
}

#[test]
fn remove_deletes_a_pack_the_browser_installed_and_refuses_bad_ids() {
    let setup = setup();
    let entry = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    install(&setup, &entry).unwrap();
    for bad in ["", "..", "../neon", "a/b", "default", "NEON"] {
        assert!(remote::remove(&setup.packs, bad).is_err(), "{bad:?}");
    }
    assert!(setup.packs.join("neon").is_dir());
    remote::remove(&setup.packs, "neon").unwrap();
    assert!(listing(&setup.packs).is_empty());
}

#[test]
fn leftover_temporary_folders_are_removed_and_other_folders_stay() {
    let setup = setup();
    std::fs::create_dir_all(setup.packs.join(".incoming-9-9/files")).unwrap();
    std::fs::write(setup.packs.join(".incoming-9-9/files/x"), b"x").unwrap();
    std::fs::write(setup.packs.join(".incoming-stray"), b"x").unwrap();
    std::fs::create_dir_all(setup.packs.join("keep")).unwrap();
    remote::clean_leftovers(&setup.packs);
    assert_eq!(listing(&setup.packs), ["keep"]);
}

/// Runs `hook` after each file download, as another process would act while an
/// install is between its steps.
struct HookTransport {
    inner: Arc<FakeTransport>,
    hook: Box<dyn Fn() + Send + Sync>,
}

impl ca_ui::pack::remote::PackTransport for HookTransport {
    fn document(
        &self,
        url: &str,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, FetchError> {
        self.inner.document(url, max_bytes, cancel)
    }

    fn file(
        &self,
        url: &str,
        destination: &std::path::Path,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), FetchError> {
        let result = self.inner.file(url, destination, max_bytes, cancel);
        (self.hook)();
        result
    }
}

fn hooked(setup: &Setup, hook: Box<dyn Fn() + Send + Sync>) -> PackServices {
    use ca_ui::pack::remote::PackTransport;
    PackServices {
        transport: Arc::new(HookTransport {
            inner: Arc::clone(&setup.transport),
            hook,
        }) as Arc<dyn PackTransport>,
        locations: locations(),
        directory: setup.packs.clone(),
    }
}

fn lock_of(folder: &std::path::Path) -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join("lock"))
        .unwrap();
    file.try_lock().unwrap();
    file
}

#[test]
fn a_cleanup_from_another_instance_leaves_a_live_install_alone() {
    let setup = setup();
    let entry = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    let packs = setup.packs.clone();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let services = hooked(
        &setup,
        Box::new(move || {
            // The cleanup of a second instance runs while this install holds
            // its work folder: the folder and the downloaded zip must stay.
            remote::clean_leftovers(&packs);
            record.lock().unwrap().push(listing(&packs));
        }),
    );
    remote::install(&services, &entry, &setup.cancel).unwrap();
    let during = seen.lock().unwrap()[0].clone();
    assert_eq!(during.len(), 1, "{during:?}");
    assert!(during[0].starts_with(".incoming-"));
    assert_eq!(listing(&setup.packs), ["neon"]);
}

#[test]
fn a_folder_another_handle_holds_locked_stays_until_the_lock_is_free() {
    let setup = setup();
    let stale = setup.packs.join(".incoming-77-1");
    std::fs::create_dir_all(stale.join("files")).unwrap();
    std::fs::write(stale.join("files/x"), b"x").unwrap();
    let held = lock_of(&stale);
    remote::clean_leftovers(&setup.packs);
    assert!(stale.join("files/x").is_file(), "a held folder is kept");
    drop(held);
    remote::clean_leftovers(&setup.packs);
    assert!(!stale.exists(), "a folder with a free lock is removed");
}

#[test]
fn a_work_folder_that_vanishes_fails_the_install_and_keeps_the_old_pack() {
    let setup = setup();
    let first = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    install(&setup, &first).unwrap();
    let second = serve_pack(
        &setup.transport,
        "neon",
        "Neon Two",
        zip_of(&[
            ("pack.json", manifest("neon", "Neon Two")),
            ("icons/16/next-difference.png", png(16)),
        ]),
    );
    let packs = setup.packs.clone();
    let services = hooked(
        &setup,
        Box::new(move || {
            for entry in std::fs::read_dir(&packs).unwrap().flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".incoming-")
                {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }),
    );
    assert!(remote::install(&services, &second, &setup.cancel).is_err());
    assert_eq!(listing(&setup.packs), ["neon"]);
    assert_eq!(
        remote::installed_by_browser(&setup.packs)["neon"],
        first.sha256
    );
}

#[test]
fn a_temporary_folder_is_not_listed_as_a_pack() {
    let setup = setup();
    std::fs::create_dir_all(setup.packs.join(".incoming-1-1")).unwrap();
    std::fs::create_dir_all(setup.packs.join("real")).unwrap();
    let found = ca_ui::pack::host::installed_folders(&setup.packs);
    assert_eq!(found.len(), 1);
    assert!(found[0].ends_with("real"));
}

#[test]
fn the_list_comes_from_the_release_of_the_running_version() {
    let setup = setup();
    let entry = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    serve_index(&setup.transport, &[entry]);
    let index = remote::fetch_index(
        setup.transport.as_ref(),
        &locations(),
        &builtin_ids(),
        &setup.cancel,
    )
    .unwrap();
    assert_eq!(index.packs.len(), 1);
    assert_eq!(
        setup.transport.requests.lock().unwrap()[0],
        "https://example.test/releases/download/v2026.10.20.0001/packs-index.json"
    );
}

#[test]
fn a_release_without_a_list_and_a_failed_read_are_told_apart() {
    let setup = setup();
    let fetch = || {
        remote::fetch_index(
            setup.transport.as_ref(),
            &locations(),
            &builtin_ids(),
            &setup.cancel,
        )
    };
    assert_eq!(fetch().unwrap_err(), IndexFailure::NoList);
    setup.transport.fail(
        &locations().index_url(),
        FetchError::Failed("offline".into()),
    );
    assert_eq!(
        fetch().unwrap_err(),
        IndexFailure::Failed("offline".to_owned())
    );
    setup
        .transport
        .put(&locations().index_url(), b"{ not json".to_vec());
    assert!(matches!(fetch().unwrap_err(), IndexFailure::Failed(_)));
}

#[test]
fn a_list_that_names_a_built_in_pack_is_refused_whole() {
    let setup = setup();
    let good = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    let builtin = serve_pack(
        &setup.transport,
        "luminous-dark",
        "Luminous",
        good_zip("luminous-dark", "Luminous"),
    );
    assert!(builtin_ids().contains(&"luminous-dark".to_owned()));
    serve_index(&setup.transport, &[good, builtin]);
    let failure = remote::fetch_index(
        setup.transport.as_ref(),
        &locations(),
        &builtin_ids(),
        &setup.cancel,
    )
    .unwrap_err();
    let IndexFailure::Failed(text) = failure else {
        panic!("expected a refusal");
    };
    assert!(text.contains("built into the program"), "{text}");
}

#[test]
fn a_preview_that_does_not_match_the_list_is_refused() {
    let setup = setup();
    let entry = serve_pack(&setup.transport, "neon", "Neon", good_zip("neon", "Neon"));
    assert!(remote::fetch_preview(
        setup.transport.as_ref(),
        &locations(),
        &entry,
        &setup.cancel
    )
    .is_ok());
    setup
        .transport
        .put(&locations().file_url(&entry.preview), png(31));
    assert!(remote::fetch_preview(
        setup.transport.as_ref(),
        &locations(),
        &entry,
        &setup.cancel
    )
    .is_err());
}
