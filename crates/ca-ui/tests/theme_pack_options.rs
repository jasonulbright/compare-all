//! The theme pack list in the options dialog is reachable by pack name.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::options::ProgramOptions;
use ca_session::AdminPolicies;
use ca_ui::options::OptionsDialog;
use ca_ui::pack::{BuiltinPack, PackHost};
use ca_ui::testing::probe::Probe;
use ca_ui::theme::{palette, Variant};
use std::time::Duration;

const MANIFEST: &[u8] = include_bytes!("fixtures/theme-pack/pack.json");

static NO_FILES: [(&str, &[u8]); 0] = [];
static BUILT_IN: [BuiltinPack; 1] = [BuiltinPack {
    manifest: br#"{"format":1,"id":"built-in-one","name":"Built In One","version":"1","modes":["light","dark"]}"#,
    files: &NO_FILES,
}];

fn write(folder: &std::path::Path, manifest: &[u8]) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("pack.json"), manifest).unwrap();
}

#[test]
fn the_options_dialog_selects_a_pack_by_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let packs = dir.path().join("packs");
    write(&packs.join("one"), MANIFEST);
    write(&packs.join("broken"), b"{ not json");
    let mut host = PackHost::new(packs, &BUILT_IN);
    let dialog = std::cell::RefCell::new(OptionsDialog::new(
        &ProgramOptions::default(),
        AdminPolicies::default(),
        1,
    ));
    dialog.borrow_mut().select_page_named("Appearance");
    let table = palette(Variant::Light);
    let mut probe = Probe::new(1280.0, 900.0);
    let mut run = |ctx: &egui::Context| {
        let _ = host.update(ctx, "");
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = dialog.borrow_mut().body(ui, &table);
        });
    };
    assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
        probe.idle(&mut run);
        probe.find("Fixture Dark").is_ok()
    }));
    for label in ["Default", "Built In One", "Fixture Dark"] {
        assert!(probe.find(label).is_ok(), "{label}: {:?}", probe.labels());
    }
    probe.click("Fixture Dark", &mut run).unwrap();
    probe.idle(&mut run);
    assert_eq!(
        dialog.borrow().options().appearance.theme_pack,
        "fixture-dark"
    );
    let frame = probe.frame(Vec::new(), &mut run);
    let texts = ca_ui::testing::probe::painted_texts(&frame);
    assert!(
        texts
            .iter()
            .any(|text| text == "This pack is dark only. The Theme setting does not apply."),
        "{texts:?}"
    );
    probe.click("Built In One", &mut run).unwrap();
    probe.idle(&mut run);
    assert_eq!(
        dialog.borrow().options().appearance.theme_pack,
        "built-in-one"
    );
    probe.click("Default", &mut run).unwrap();
    probe.idle(&mut run);
    assert_eq!(dialog.borrow().options().appearance.theme_pack, "");
}

#[test]
fn a_refused_pack_is_listed_with_its_reason_and_cannot_be_selected() {
    let dir = tempfile::tempdir().unwrap();
    let packs = dir.path().join("packs");
    write(&packs.join("broken"), b"{ not json");
    let mut host = PackHost::new(packs, &[]);
    let dialog = std::cell::RefCell::new(OptionsDialog::new(
        &ProgramOptions::default(),
        AdminPolicies::default(),
        1,
    ));
    dialog.borrow_mut().select_page_named("Appearance");
    let table = palette(Variant::Light);
    let mut probe = Probe::new(1280.0, 900.0);
    let mut run = |ctx: &egui::Context| {
        let _ = host.update(ctx, "");
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = dialog.borrow_mut().body(ui, &table);
        });
    };
    let mut texts = Vec::new();
    assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
        let frame = probe.frame(Vec::new(), &mut run);
        texts = ca_ui::testing::probe::painted_texts(&frame);
        texts.iter().any(|text| text.contains("Not usable"))
    }));
    assert!(texts.iter().any(|text| text == "broken"), "{texts:?}");
    assert!(
        probe.find("broken").is_err(),
        "a refused pack is not a button"
    );
}
