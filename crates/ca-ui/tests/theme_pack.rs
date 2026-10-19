//! Theme pack manifest, limits, loading, fallback and painting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::options::{ColorGroup, PaletteOptions, Rgb};
use ca_ui::icons::Icon;
use ca_ui::pack::{
    self, load, BuiltinPack, DirectoryFiles, MemoryFiles, PackHost, PackManifest, PackModes,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MANIFEST: &[u8] = include_bytes!("fixtures/theme-pack/pack.json");
const RED: [u8; 4] = [200, 20, 30, 255];

/// A solid square PNG.
fn png(size: u32, pixel: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(size, size, image::Rgba(pixel));
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

/// The files the fixture manifest refers to: `find` at 16 and 24, `file` at 16
/// only, and `stop` at 24 through an explicit path.
fn fixture_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("icons/16/find.png", png(16, RED)),
        ("icons/24/find.png", png(24, RED)),
        ("icons/16/file.png", png(16, RED)),
        ("special/stop-24.png", png(24, [10, 200, 10, 255])),
    ]
}

fn write_pack(root: &Path, manifest: &[u8], files: &[(&str, Vec<u8>)]) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("pack.json"), manifest).unwrap();
    for (path, bytes) in files {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
}

fn manifest_with(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(MANIFEST).unwrap();
    edit(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn refused(manifest: &[u8], files: &MemoryFiles) -> String {
    load::load(manifest, files)
        .unwrap_err()
        .message()
        .to_owned()
}

fn memory(files: Vec<(&'static str, Vec<u8>)>) -> MemoryFiles {
    let mut memory = MemoryFiles::new();
    for (path, bytes) in files {
        memory.insert(path, bytes);
    }
    memory
}

#[test]
fn the_fixture_manifest_is_read() {
    let manifest = PackManifest::parse(MANIFEST).unwrap();
    assert_eq!(manifest.id, "fixture-dark");
    assert_eq!(
        manifest.pack_modes(),
        PackModes {
            light: false,
            dark: true
        }
    );
    assert_eq!(
        manifest.icon_path(Icon::Find, 16).as_deref(),
        Some("icons/16/find.png")
    );
    assert_eq!(
        manifest.icon_path(Icon::Stop, 24).as_deref(),
        Some("special/stop-24.png")
    );
    assert_eq!(
        manifest.icon_path(Icon::Stop, 16).as_deref(),
        Some("icons/16/stop.png")
    );
}

#[test]
fn a_pack_with_one_mode_overrides_the_choice() {
    let dark = PackModes {
        light: false,
        dark: true,
    };
    assert!(dark.dark_for(false));
    assert!(dark.dark_for(true));
    assert!(!dark.offers_choice());
    assert!(PackModes::BOTH.offers_choice());
    assert!(!PackModes::BOTH.dark_for(false));
    assert!(PackModes::BOTH.dark_for(true));
}

#[test]
fn colors_of_the_person_win_over_colors_of_the_pack() {
    let manifest = PackManifest::parse(MANIFEST).unwrap();
    let mut user = PaletteOptions::default();
    user.group_mut(ColorGroup::Text)
        .table_mut(true)
        .set("same_line", Rgb::new(1, 2, 3));
    let layered = pack::layered(&manifest.colors, &user);
    let table = layered.group(ColorGroup::Text).table(true);
    assert_eq!(table.get("same_line"), Some(Rgb::new(1, 2, 3)));
    assert_eq!(
        table.get("important_line"),
        Some(Rgb::new(0x40, 0x10, 0x10))
    );
    assert_eq!(
        layered
            .group(ColorGroup::Text)
            .table(false)
            .get("same_line"),
        None
    );
}

#[test]
fn a_pack_in_a_folder_loads_with_colored_icons_and_falls_back_for_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    write_pack(dir.path(), MANIFEST, &fixture_files());
    let files = DirectoryFiles::new(dir.path()).unwrap();
    let loaded = load::load(MANIFEST, &files).unwrap();
    assert_eq!(loaded.image_count(), 4);
    assert!(loaded.has_icon(Icon::Find));
    assert!(loaded.has_icon(Icon::Stop));
    assert!(!loaded.has_icon(Icon::Save), "a missing icon has no image");
    assert_eq!(loaded.size_for(Icon::Find, 16), Some(16));
    assert_eq!(loaded.size_for(Icon::Find, 20), Some(24));
    assert_eq!(
        loaded.size_for(Icon::Find, 40),
        Some(24),
        "largest when none is big enough"
    );
    assert_eq!(loaded.size_for(Icon::File, 24), Some(16));
    assert_eq!(loaded.size_for(Icon::Save, 24), None);

    let ctx = egui::Context::default();
    let texture = loaded.texture(&ctx, Icon::Find, 16).unwrap();
    assert_eq!(
        texture.id(),
        loaded.texture(&ctx, Icon::Find, 16).unwrap().id()
    );
    assert_eq!(
        texture.id(),
        loaded.texture(&ctx, Icon::Find, 12).unwrap().id(),
        "one upload serves every target that picks the same size"
    );
    assert!(loaded.texture(&ctx, Icon::Save, 16).is_none());
}

#[test]
fn built_in_bytes_load_the_same_way() {
    let loaded = load::load(MANIFEST, &memory(fixture_files())).unwrap();
    assert_eq!(loaded.id(), "fixture-dark");
    assert_eq!(loaded.image_count(), 4);
}

#[test]
fn a_manifest_that_breaks_a_rule_is_refused_with_a_message() {
    let none = MemoryFiles::new();
    let huge = vec![b' '; ca_ui::pack::manifest::MAX_MANIFEST_BYTES + 1];
    assert!(refused(&huge, &none).contains("The limit is"));
    assert!(refused(b"{", &none).contains("not valid"));
    for (what, edit, expected) in [
        ("format", serde_json::json!({"format": 2}), "format 2"),
        ("id", serde_json::json!({"id": "Bad Id"}), "pack id"),
        ("id", serde_json::json!({"id": "default"}), "pack id"),
        ("name", serde_json::json!({"name": ""}), "pack name"),
        ("modes", serde_json::json!({"modes": []}), "no mode"),
        (
            "modes",
            serde_json::json!({"modes": ["dark", "dark"]}),
            "mode twice",
        ),
        (
            "icons",
            serde_json::json!({"icons": {"sizes": [20]}}),
            "Icon size 20",
        ),
        (
            "icons",
            serde_json::json!({"icons": {"sizes": [16], "pattern": "../{size}/{name}.png"}}),
            "plain relative path",
        ),
        (
            "icons",
            serde_json::json!({"icons": {"sizes": [16], "pattern": "icons/x.png"}}),
            "needs both",
        ),
        (
            "icons",
            serde_json::json!({"icons": {"sizes": [16], "files": {"find": {"16": "/etc/x.png"}}}}),
            "plain relative path",
        ),
        (
            "icons",
            serde_json::json!({"icons": {"sizes": [16], "files": {"find": {"24": "a.png"}}}}),
            "does not list",
        ),
        (
            "colors",
            serde_json::json!({"colors": {"text": {"dark": {"slots": {"same_line": "red"}}}}}),
            "#RRGGBB",
        ),
    ] {
        let manifest = manifest_with(|value| {
            for (key, replacement) in edit.as_object().unwrap() {
                value[key] = replacement.clone();
            }
        });
        let message = refused(&manifest, &none);
        assert!(message.contains(expected), "{what}: {message}");
    }
}

#[test]
fn a_manifest_with_too_many_file_entries_is_refused() {
    let mut files = serde_json::Map::new();
    for index in 0..=ca_ui::pack::manifest::MAX_FILE_ENTRIES {
        files.insert(format!("icon{index}"), serde_json::json!({"16": "a.png"}));
    }
    let manifest = manifest_with(|value| {
        value["icons"] = serde_json::json!({"sizes": [16], "files": files});
    });
    assert!(refused(&manifest, &MemoryFiles::new()).contains("more than"));
}

#[test]
fn too_many_color_slots_are_refused() {
    let mut slots = serde_json::Map::new();
    for index in 0..=ca_ui::pack::manifest::MAX_COLOR_SLOTS {
        slots.insert(format!("slot{index}"), serde_json::json!("#000000"));
    }
    let manifest = manifest_with(|value| {
        value["colors"] = serde_json::json!({"text": {"dark": {"slots": slots}}});
    });
    assert!(refused(&manifest, &MemoryFiles::new()).contains("colors"));
}

#[test]
fn an_icon_file_that_breaks_a_limit_refuses_the_pack() {
    let mut files = fixture_files();
    files.push(("icons/32/find.png", png(16, RED)));
    let message = refused(MANIFEST, &memory(files));
    assert!(message.contains("16 by 16"), "{message}");
    assert!(message.contains("32 by 32"), "{message}");

    let mut files = fixture_files();
    files.push(("icons/16/copy.png", b"not a png".to_vec()));
    assert!(refused(MANIFEST, &memory(files)).contains("not a PNG"));

    let mut files = fixture_files();
    files.push(("icons/16/copy.png", vec![0u8; load::MAX_FILE_BYTES + 1]));
    assert!(refused(MANIFEST, &memory(files)).contains("larger than"));

    let mut files = fixture_files();
    files.push(("icons/16/copy.png", png(300, RED)));
    assert!(refused(MANIFEST, &memory(files)).contains("300 by 300"));
}

#[test]
fn icon_files_past_the_total_limit_refuse_the_pack() {
    let junk = vec![0u8; 127 * 1024];
    let mut bytes = png(16, RED);
    bytes.extend_from_slice(&junk);
    let mut files = MemoryFiles::new();
    for icon in Icon::ALL {
        files.insert(&format!("icons/16/{}.png", icon.id()), bytes.clone());
    }
    let manifest = manifest_with(|value| {
        value["icons"] = serde_json::json!({"sizes": [16]});
    });
    let message = refused(&manifest, &files);
    assert!(message.contains("together"), "{message}");
}

#[test]
fn a_directory_source_refuses_paths_that_leave_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    write_pack(dir.path(), MANIFEST, &fixture_files());
    std::fs::write(
        dir.path().parent().unwrap().join("outside.png"),
        png(16, RED),
    )
    .unwrap();
    let files = DirectoryFiles::new(dir.path()).unwrap();
    for path in [
        "../outside.png",
        "/outside.png",
        "icons\\16\\find.png",
        "a//b",
        "C:/x.png",
    ] {
        assert!(
            pack::PackFiles::read(&files, path, 1024).is_err(),
            "{path} was read"
        );
    }
    assert!(pack::PackFiles::read(&files, "icons/16/none.png", 1024)
        .unwrap()
        .is_none());
}

#[cfg(unix)]
#[test]
fn a_directory_source_does_not_follow_a_link_out_of_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_pack(dir.path(), MANIFEST, &fixture_files());
    std::fs::write(outside.path().join("find.png"), png(16, RED)).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
    let files = DirectoryFiles::new(dir.path()).unwrap();
    assert!(pack::PackFiles::read(&files, "linked/find.png", 4096).is_err());
}

static BUILT_IN_FILES: [(&str, &[u8]); 0] = [];
static BUILT_IN: [BuiltinPack; 1] = [BuiltinPack {
    manifest: br#"{"format":1,"id":"built-in-one","name":"Built In One","version":"1","modes":["light","dark"]}"#,
    files: &BUILT_IN_FILES,
}];

fn wait_for(
    ctx: &egui::Context,
    host: &mut PackHost,
    wanted: &str,
    done: impl Fn(&PackHost, Option<&Arc<pack::LoadedPack>>) -> bool,
) -> Option<Arc<pack::LoadedPack>> {
    let mut last = None;
    assert!(
        ca_ui::testing::wait_until(Duration::from_secs(20), || {
            last = host.update(ctx, wanted);
            done(host, last.as_ref())
        }),
        "the pack host never reached the wanted state"
    );
    last
}

fn packs_folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let packs = dir.path().join("packs");
    write_pack(&packs.join("one"), MANIFEST, &fixture_files());
    write_pack(&packs.join("broken"), b"{ not json", &[]);
    write_pack(
        &packs.join("clash"),
        &manifest_with(|v| v["id"] = "built-in-one".into()),
        &[],
    );
    (dir, packs)
}

#[test]
fn the_host_lists_packs_loads_the_selected_one_and_installs_it_for_painting() {
    let (_dir, packs) = packs_folder();
    let mut host = PackHost::new(packs, &BUILT_IN);
    let ctx = egui::Context::default();
    wait_for(&ctx, &mut host, "", |_, _| pack::catalog(&ctx).scanned);

    let catalog = pack::catalog(&ctx);
    let names: Vec<&str> = catalog.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names[0], "Default");
    assert!(names.contains(&"Built In One"));
    assert!(names.contains(&"Fixture Dark"));
    let broken = catalog.entries.iter().find(|e| e.name == "broken").unwrap();
    assert!(broken.problem.as_deref().unwrap().contains("not valid"));
    let clash = catalog.entries.iter().find(|e| e.name == "Fixture Dark");
    assert!(clash.is_some());
    assert!(catalog
        .entries
        .iter()
        .any(|e| e.problem.as_deref() == Some("Another pack already uses this identifier.")));
    assert!(pack::active(&ctx).is_none());

    let loaded = wait_for(&ctx, &mut host, "fixture-dark", |_, pack| pack.is_some()).unwrap();
    assert_eq!(loaded.id(), "fixture-dark");
    assert!(pack::active(&ctx).is_some());

    assert!(host.update(&ctx, "").is_none());
    assert!(
        pack::active(&ctx).is_none(),
        "the empty id selects the built-in look"
    );

    wait_for(&ctx, &mut host, "fixture-dark", |_, pack| pack.is_some());
    assert!(host.update(&ctx, "gone").is_none());
    assert!(
        pack::active(&ctx).is_none(),
        "an unknown id selects the built-in look"
    );
}

#[test]
fn a_pack_that_fails_to_load_is_marked_and_the_built_in_look_stays() {
    let dir = tempfile::tempdir().unwrap();
    let packs = dir.path().join("packs");
    let mut files = fixture_files();
    files.push(("icons/32/find.png", b"not a png".to_vec()));
    write_pack(&packs.join("bad"), MANIFEST, &files);
    let mut host = PackHost::new(packs, &[]);
    let ctx = egui::Context::default();
    wait_for(&ctx, &mut host, "fixture-dark", |_, _| {
        pack::catalog(&ctx)
            .find("fixture-dark")
            .is_some_and(|entry| entry.problem.is_some())
    });
    assert!(pack::active(&ctx).is_none());
    let problem = pack::catalog(&ctx)
        .find("fixture-dark")
        .unwrap()
        .problem
        .clone();
    assert!(problem.unwrap().contains("not a PNG"));
}

#[test]
fn a_colored_icon_paints_untinted_and_an_icon_the_pack_lacks_paints_as_a_mask() {
    let loaded = Arc::new(load::load(MANIFEST, &memory(fixture_files())).unwrap());
    let ctx = egui::Context::default();
    pack::install_active(&ctx, Some(Arc::clone(&loaded)));
    let tint = egui::Color32::from_rgb(250, 10, 10);
    let output = ctx.run(ca_ui::testing::raw_input(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let painter = ui.painter();
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(16.0, 16.0));
            Icon::Find.paint(painter, rect, tint);
            Icon::Save.paint(painter, rect.translate(egui::vec2(30.0, 0.0)), tint);
        });
    });
    let meshes: Vec<&egui::Mesh> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) => Some(&**mesh),
            _ => None,
        })
        .collect();
    let find = loaded.texture(&ctx, Icon::Find, 16).unwrap();
    let colored = meshes
        .iter()
        .find(|mesh| mesh.texture_id == find.id())
        .unwrap();
    assert!(colored
        .vertices
        .iter()
        .all(|vertex| vertex.color == egui::Color32::WHITE));
    let mask = meshes
        .iter()
        .find(|mesh| mesh.texture_id != find.id() && !mesh.vertices.is_empty())
        .unwrap();
    assert!(mask.vertices.iter().all(|vertex| vertex.color == tint));

    pack::install_active(&ctx, None);
    let output = ctx.run(ca_ui::testing::raw_input(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(16.0, 16.0));
            Icon::Find.paint(ui.painter(), rect, tint);
        });
    });
    assert!(output.shapes.iter().all(|shape| match &shape.shape {
        egui::Shape::Mesh(mesh) => mesh.texture_id != find.id(),
        _ => true,
    }));
}

#[test]
fn windows_device_names_are_refused_in_every_path_form() {
    let dir = tempfile::tempdir().unwrap();
    write_pack(dir.path(), MANIFEST, &fixture_files());
    let files = DirectoryFiles::new(dir.path()).unwrap();
    for path in [
        "NUL",
        "nul.png",
        "icons/16/Con.png",
        "icons/aux.tar.png",
        "icons/PRN.",
        "icons/com1.png",
        "icons/COM9",
        "lpt1.png",
        "icons/LPT9.x.png",
    ] {
        assert!(
            pack::manifest::check_path(path).is_err(),
            "{path} was accepted"
        );
        assert!(
            pack::PackFiles::read(&files, path, 1024).is_err(),
            "{path} was read"
        );
    }
    for path in [
        "icons/16/console.png",
        "icons/16/com0.png",
        "icons/null-1.png",
    ] {
        assert!(
            pack::manifest::check_path(path).is_ok(),
            "{path} was refused"
        );
    }
    let manifest = manifest_with(|v| v["icons"]["files"]["stop"]["24"] = "special/NUL.png".into());
    let message = refused(&manifest, &memory(fixture_files()));
    assert!(message.contains("NUL.png"), "{message}");
}

#[test]
fn a_folder_with_very_many_subfolders_is_read_up_to_a_bound() {
    let dir = tempfile::tempdir().unwrap();
    let total = pack::host::MAX_SCANNED_ENTRIES + 200;
    for index in 0..total {
        std::fs::create_dir(dir.path().join(format!("d{index:05}"))).unwrap();
    }
    let folders = pack::host::installed_folders(dir.path());
    assert_eq!(folders.len(), pack::host::MAX_INSTALLED_PACKS);
    let mut sorted = folders.clone();
    sorted.sort();
    assert_eq!(folders, sorted);
}

#[test]
fn a_pack_whose_manifest_changed_after_the_scan_is_not_applied_under_the_old_id() {
    let (_dir, packs) = packs_folder();
    let mut host = PackHost::new(packs.clone(), &BUILT_IN);
    let ctx = egui::Context::default();
    wait_for(&ctx, &mut host, "", |_, _| pack::catalog(&ctx).scanned);
    std::fs::write(
        packs.join("one").join("pack.json"),
        manifest_with(|v| v["id"] = "renamed".into()),
    )
    .unwrap();
    wait_for(&ctx, &mut host, "fixture-dark", |_, _| {
        pack::catalog(&ctx).find("renamed").is_some()
    });
    assert!(pack::active(&ctx).is_none());
    assert!(pack::catalog(&ctx).find("fixture-dark").is_none());
    assert!(host.update(&ctx, "fixture-dark").is_none());
}

#[test]
fn a_colored_icon_in_a_disabled_ui_paints_dimmed() {
    let loaded = Arc::new(load::load(MANIFEST, &memory(fixture_files())).unwrap());
    let ctx = egui::Context::default();
    pack::install_active(&ctx, Some(Arc::clone(&loaded)));
    let output = ctx.run(ca_ui::testing::raw_input(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_enabled_ui(false, |ui| {
                let rect =
                    egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(16.0, 16.0));
                Icon::Find.paint(ui.painter(), rect, egui::Color32::WHITE);
            });
        });
    });
    let find = loaded.texture(&ctx, Icon::Find, 16).unwrap();
    let colored = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == find.id() => Some(mesh.clone()),
            _ => None,
        })
        .unwrap();
    assert!(colored
        .vertices
        .iter()
        .all(|vertex| vertex.color != egui::Color32::WHITE));
}
