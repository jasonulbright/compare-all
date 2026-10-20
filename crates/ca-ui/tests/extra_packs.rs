//! The packs shipped as folders under `assets/packs` and not compiled into the
//! program: each loads through the same loader as an installed pack, has every
//! icon at every size, and keeps AA contrast for every text color.

#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use ca_ui::icons::Icon;
use ca_ui::pack::{load, DirectoryFiles, LoadedPack, BUILTIN};
use common::{contrast, AA_NORMAL_TEXT, SIZES};
use std::path::PathBuf;

/// Folder name, display name, and whether the pack is dark.
const FOLDERS: &[(&str, &str, bool)] = &[
    ("flat-outline", "Flat Outline", false),
    ("pale-rim", "Pale Rim", false),
    ("vivid-outline", "Vivid Outline", false),
    ("soft-puff", "Soft Puff", false),
    ("sea-glass", "Sea Glass", false),
    ("slate-raised", "Slate Raised", false),
    ("neon-tile", "Neon Tile", true),
    ("glow-tile", "Glow Tile", true),
    ("teal-night", "Teal Night", true),
];

fn folder(id: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/packs")
        .join(id)
}

fn load_folder(id: &str) -> LoadedPack {
    let root = folder(id);
    let files = DirectoryFiles::new(&root).unwrap();
    let manifest = std::fs::read(root.join("pack.json")).unwrap();
    load::load(&manifest, &files)
        .map_err(|error| format!("{id}: {error}"))
        .unwrap()
}

#[test]
fn every_folder_loads_with_its_id_name_and_mode() {
    let mut ids = Vec::new();
    for (id, name, dark) in FOLDERS {
        let pack = load_folder(id);
        assert_eq!(pack.id(), *id);
        assert_eq!(pack.name(), *name);
        assert_eq!(pack.modes().dark, *dark, "{id}");
        assert_eq!(pack.modes().light, !*dark, "{id}");
        assert!(!id.contains("default"), "{id}");
        ids.push(pack.id().to_owned());
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), FOLDERS.len());
    for builtin in BUILTIN {
        let manifest = String::from_utf8_lossy(builtin.manifest);
        for id in &ids {
            assert!(!manifest.contains(&format!("\"id\": \"{id}\"")), "{id}");
        }
    }
}

#[test]
fn every_app_icon_is_present_at_every_size() {
    for (id, _, _) in FOLDERS {
        let pack = load_folder(id);
        for icon in Icon::ALL {
            assert!(pack.has_icon(*icon), "{id} lacks {}", icon.id());
            for size in SIZES {
                let path = folder(id)
                    .join("icons")
                    .join(size.to_string())
                    .join(format!("{}.png", icon.id()));
                assert!(path.is_file(), "{}", path.display());
            }
        }
        assert_eq!(pack.image_count(), Icon::ALL.len() * SIZES.len(), "{id}");
        for size in SIZES {
            let count = std::fs::read_dir(folder(id).join("icons").join(size.to_string()))
                .unwrap()
                .count();
            assert_eq!(count, Icon::ALL.len(), "{id} {size}");
        }
    }
}

#[test]
fn text_colors_keep_aa_contrast_against_their_backgrounds() {
    let mut failures = Vec::new();
    for (id, _, _) in FOLDERS {
        let (lowest, line, failed) = contrast(&load_folder(id));
        println!("LOWEST {id} {lowest:.2} ({line})");
        failures.extend(failed);
    }
    assert!(failures.is_empty(), "below {AA_NORMAL_TEXT}: {failures:#?}");
}
