//! The packs compiled into the program: every icon is present at every size,
//! the packs load, and each text color keeps AA contrast against its background.

#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use ca_ui::icons::Icon;
use ca_ui::pack::{load, BuiltinPack, MemoryFiles, BUILTIN};
use common::{contrast, SIZES};

fn memory_files(pack: &BuiltinPack) -> MemoryFiles {
    let mut files = MemoryFiles::new();
    for (path, bytes) in pack.files {
        files.insert(path, *bytes);
    }
    files
}

#[test]
fn two_packs_are_built_in_and_load() {
    let ids: Vec<String> = BUILTIN
        .iter()
        .map(|p| {
            load::load(p.manifest, &memory_files(p))
                .map(|l| l.id().to_owned())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(ids, ["luminous-dark", "bold-outline"]);
}

#[test]
fn every_app_icon_is_present_at_every_size() {
    for builtin in BUILTIN {
        let pack = load::load(builtin.manifest, &memory_files(builtin)).unwrap();
        let mut missing = Vec::new();
        for icon in Icon::ALL {
            for size in SIZES {
                let path = format!("icons/{size}/{}.png", icon.id());
                if !builtin.files.iter().any(|(name, _)| *name == path) {
                    missing.push(path);
                }
            }
            assert!(pack.has_icon(*icon), "{} lacks {}", pack.id(), icon.id());
        }
        assert!(missing.is_empty(), "{} lacks {missing:?}", pack.id());
        assert_eq!(builtin.files.len(), Icon::ALL.len() * SIZES.len());
        assert_eq!(pack.image_count(), Icon::ALL.len() * SIZES.len());
        for (name, bytes) in builtin.files {
            assert!(bytes.len() < 128 * 1024, "{name} is too large");
        }
    }
}

#[test]
fn text_colors_keep_aa_contrast_against_their_backgrounds() {
    let mut failures = Vec::new();
    for builtin in BUILTIN {
        let pack = load::load(builtin.manifest, &memory_files(builtin)).unwrap();
        failures.extend(contrast(&pack).2);
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
