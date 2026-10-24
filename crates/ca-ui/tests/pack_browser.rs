//! The pack browser in Options > Appearance: every control is reachable by its
//! accessible label, installs and removals show in the theme pack list, and a
//! policy turns the button off.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

mod pack_net;

use ca_session::options::ProgramOptions;
use ca_session::{AdminPolicies, InMemoryPolicySource, PolicyKey, PolicyLoader};
use ca_ui::options::OptionsDialog;
use ca_ui::pack::browser::{
    CLOSE_LABEL, DOWNLOAD_FAILED_TEXT, LIST_FAILED_TEXT, NO_LIST_TEXT, OPEN_LABEL, PAGE_LABEL,
    POLICY_TEXT,
};
use ca_ui::pack::remote::FetchError;
use ca_ui::pack::PackHost;
use ca_ui::testing::probe::{painted_texts, Probe};
use ca_ui::theme::{palette, Variant};
use pack_net::FakeTransport;
use pack_net::{good_zip, listing, locations, manifest, serve_index, serve_pack, services};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

struct Harness {
    _dir: tempfile::TempDir,
    packs: std::path::PathBuf,
    transport: Arc<FakeTransport>,
    host: RefCell<PackHost>,
    dialog: RefCell<OptionsDialog>,
    probe: Probe,
}

impl Harness {
    fn new(policies: AdminPolicies) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let packs = dir.path().join("packs");
        let transport = Arc::new(FakeTransport::default());
        let mut dialog = OptionsDialog::new(&ProgramOptions::default(), policies, 1);
        dialog.set_pack_services(Some(services(&transport, &packs)));
        dialog.select_page_named("Appearance");
        Self {
            host: RefCell::new(PackHost::new(packs.clone(), &[])),
            packs,
            transport,
            dialog: RefCell::new(dialog),
            probe: Probe::new(1280.0, 1000.0),
            _dir: dir,
        }
    }

    fn frame(&mut self) -> Vec<String> {
        let table = palette(Variant::Light);
        let host = &self.host;
        let dialog = &self.dialog;
        let mut run = |ctx: &egui::Context| {
            let _ = host.borrow_mut().update(ctx, "");
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = dialog.borrow_mut().body(ui, &table);
            });
            dialog.borrow_mut().show_pack_browser(ctx);
        };
        painted_texts(&self.probe.frame(Vec::new(), &mut run))
    }

    fn wait(&mut self, mut done: impl FnMut(&Self, &[String]) -> bool) {
        let mut ok = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            let texts = self.frame();
            if done(self, &texts) {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ok, "timed out; controls: {:?}", self.probe.labels());
    }

    fn click(&mut self, label: &str) {
        let table = palette(Variant::Light);
        let host = &self.host;
        let dialog = &self.dialog;
        let mut run = |ctx: &egui::Context| {
            let _ = host.borrow_mut().update(ctx, "");
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = dialog.borrow_mut().body(ui, &table);
            });
            dialog.borrow_mut().show_pack_browser(ctx);
        };
        self.probe.click(label, &mut run).unwrap();
    }

    fn has(&self, label: &str) -> bool {
        self.probe.find(label).is_ok()
    }
}

fn two_packs(transport: &FakeTransport) {
    let neon = serve_pack(transport, "neon", "Neon", good_zip("neon", "Neon"));
    let sea = serve_pack(transport, "sea", "Sea", good_zip("sea", "Sea"));
    serve_index(transport, &[neon, sea]);
}

#[test]
fn every_control_is_reachable_by_its_label_and_an_install_shows_in_the_list() {
    let mut h = Harness::new(AdminPolicies::default());
    two_packs(&h.transport);
    h.wait(|h, _| h.has(OPEN_LABEL));
    assert!(h.probe.find(OPEN_LABEL).unwrap().enabled);
    h.click(OPEN_LABEL);
    h.wait(|h, _| h.has("Install Neon") && h.has("Install Sea") && h.has(CLOSE_LABEL));
    assert!(h.probe.find("Install Neon").unwrap().enabled);

    h.click("Install Neon");
    h.wait(|h, _| h.has("Remove Neon"));
    assert!(h.has("Install Sea"));
    assert_eq!(listing(&h.packs), ["neon"]);
    h.wait(|h, _| h.has("Neon"));
    let dialog_pack = h.dialog.borrow().options().appearance.theme_pack.clone();
    assert_eq!(dialog_pack, "", "the browser does not select the pack");

    h.click("Remove Neon");
    h.wait(|h, _| h.has("Install Neon"));
    assert!(listing(&h.packs).is_empty());
    h.wait(|h, _| !h.has("Neon"));

    h.click(CLOSE_LABEL);
    h.wait(|h, _| !h.has(CLOSE_LABEL));
    assert!(h.has(OPEN_LABEL));
}

#[test]
fn the_row_shows_the_name_the_mode_and_the_size() {
    let mut h = Harness::new(AdminPolicies::default());
    two_packs(&h.transport);
    h.wait(|h, _| h.has(OPEN_LABEL));
    h.click(OPEN_LABEL);
    h.wait(|_, texts| {
        texts
            .iter()
            .any(|t| t.starts_with("Dark, ") && t.ends_with(" KB"))
    });
}

#[test]
fn a_failed_zip_download_says_so_and_offers_the_release_page() {
    let mut h = Harness::new(AdminPolicies::default());
    two_packs(&h.transport);
    h.transport.fail(
        &locations().file_url("pack-neon.zip"),
        FetchError::Failed("the server answered with text/html, not a file".to_owned()),
    );
    h.wait(|h, _| h.has(OPEN_LABEL));
    h.click(OPEN_LABEL);
    h.wait(|h, _| h.has("Install Neon"));
    h.click("Install Neon");
    h.wait(|_, texts| texts.iter().any(|t| t.starts_with(DOWNLOAD_FAILED_TEXT)));
    assert!(h.has("Install Neon"), "the row stays usable");
    assert!(h.has("Install Sea"));
    h.click(PAGE_LABEL);
    h.frame();
    assert_eq!(
        h.dialog.borrow_mut().take_page_request().as_deref(),
        Some("https://example.test/releases/tag/v2026.10.20.0001")
    );
    assert!(h.dialog.borrow_mut().take_page_request().is_none());
    assert!(listing(&h.packs).is_empty());
}

#[test]
fn a_refused_pack_shows_the_reason_and_leaves_nothing() {
    let mut h = Harness::new(AdminPolicies::default());
    let entry = serve_pack(&h.transport, "neon", "Neon", good_zip("other", "Other"));
    serve_index(&h.transport, &[entry]);
    h.wait(|h, _| h.has(OPEN_LABEL));
    h.click(OPEN_LABEL);
    h.wait(|h, _| h.has("Install Neon"));
    h.click("Install Neon");
    h.wait(|_, texts| texts.iter().any(|t| t.contains("another identifier")));
    assert!(listing(&h.packs).is_empty());
}

#[test]
fn a_failed_list_read_shows_the_error_and_offers_the_release_page() {
    let mut h = Harness::new(AdminPolicies::default());
    h.transport.fail(
        &locations().index_url(),
        FetchError::Failed("offline".to_owned()),
    );
    h.wait(|h, _| h.has(OPEN_LABEL));
    h.click(OPEN_LABEL);
    h.wait(|h, texts| {
        h.has(PAGE_LABEL)
            && texts.iter().any(|t| t == LIST_FAILED_TEXT)
            && texts.iter().any(|t| t == "offline")
    });
    h.click(PAGE_LABEL);
    h.frame();
    assert!(h.dialog.borrow_mut().take_page_request().is_some());
    h.click(CLOSE_LABEL);
    h.wait(|h, _| !h.has(CLOSE_LABEL));
}

#[test]
fn a_build_without_a_release_shows_that_no_list_exists() {
    let mut h = Harness::new(AdminPolicies::default());
    h.wait(|h, _| h.has(OPEN_LABEL));
    h.click(OPEN_LABEL);
    h.wait(|_, texts| texts.iter().any(|t| t == NO_LIST_TEXT));
    assert!(!h.has(PAGE_LABEL));
}

#[test]
fn a_pack_copied_by_hand_stays_when_the_list_names_the_same_id() {
    let mut h = Harness::new(AdminPolicies::default());
    two_packs(&h.transport);
    let folder = h.packs.join("neon");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("pack.json"), manifest("neon", "Mine")).unwrap();
    h.wait(|h, _| h.has(OPEN_LABEL));
    h.click(OPEN_LABEL);
    h.wait(|h, _| h.has("Install Neon"));
    assert!(!h.has("Remove Neon"), "a hand-copied pack has no Remove");
    h.click("Install Neon");
    h.wait(|_, texts| texts.iter().any(|t| t.contains("not changed")));
    assert!(folder.join("pack.json").is_file());
    assert!(!folder.join(".downloaded").exists());
}

#[test]
fn a_policy_turns_the_button_off_and_says_why() {
    let policies: AdminPolicies = InMemoryPolicySource::new()
        .with(PolicyKey::DisableThemePackDownloads, true)
        .load()
        .unwrap();
    let mut h = Harness::new(policies);
    two_packs(&h.transport);
    let mut texts = Vec::new();
    h.wait(|h, shown| {
        texts = shown.to_vec();
        h.has(OPEN_LABEL)
    });
    assert!(!h.probe.find(OPEN_LABEL).unwrap().enabled);
    assert!(texts.iter().any(|t| t == POLICY_TEXT), "{texts:?}");
    let table = palette(Variant::Light);
    let host = &h.host;
    let dialog = &h.dialog;
    let mut run = |ctx: &egui::Context| {
        let _ = host.borrow_mut().update(ctx, "");
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = dialog.borrow_mut().body(ui, &table);
        });
        dialog.borrow_mut().show_pack_browser(ctx);
    };
    let button = h.probe.find(OPEN_LABEL).unwrap();
    h.probe.press_at(button.rect.center(), &mut run);
    h.probe.idle(&mut run);
    assert!(!h.has(CLOSE_LABEL), "the browser did not open");
    assert!(h.transport.requests.lock().unwrap().is_empty());
}

#[test]
fn the_button_is_off_without_services() {
    let dialog = RefCell::new(OptionsDialog::new(
        &ProgramOptions::default(),
        AdminPolicies::default(),
        1,
    ));
    dialog.borrow_mut().select_page_named("Appearance");
    let table = palette(Variant::Light);
    let mut probe = Probe::new(1280.0, 1000.0);
    let mut run = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = dialog.borrow_mut().body(ui, &table);
        });
    };
    probe.idle(&mut run);
    assert!(!probe.find(OPEN_LABEL).unwrap().enabled);
}
