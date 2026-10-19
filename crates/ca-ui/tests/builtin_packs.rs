//! The packs compiled into the program: every icon is present at every size,
//! the packs load, and each text color keeps AA contrast against its background.

#![allow(clippy::unwrap_used, missing_docs)]

use ca_session::options::{ColorGroup, PaletteOptions};
use ca_ui::icons::Icon;
use ca_ui::pack::{self, load, BuiltinPack, MemoryFiles, BUILTIN};
use ca_ui::theme::slots::color_of;
use ca_ui::theme::Variant;
use egui::Color32;

const SIZES: [u32; 4] = [16, 24, 32, 48];
const AA_NORMAL_TEXT: f64 = 4.5;

fn memory_files(pack: &BuiltinPack) -> MemoryFiles {
    let mut files = MemoryFiles::new();
    for (path, bytes) in pack.files {
        files.insert(path, *bytes);
    }
    files
}

fn channel(value: u8) -> f64 {
    let c = f64::from(value) / 255.0;
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(color: Color32) -> f64 {
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

fn ratio(a: Color32, b: Color32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
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

type Pair = (ColorGroup, &'static str, &'static str);

const PAIRS: &[Pair] = &[
    (ColorGroup::Text, "same_text", "same_line"),
    (ColorGroup::Text, "same_text_other", "same_line_other"),
    (ColorGroup::Text, "same_text", "stripe"),
    (ColorGroup::Text, "same_text", "selection"),
    (ColorGroup::Text, "important_text", "caret_line"),
    (ColorGroup::Text, "important_text", "important_line"),
    (ColorGroup::Text, "unimportant_text", "unimportant_line"),
    (ColorGroup::Text, "orphan_text", "orphan_line"),
    (ColorGroup::Text, "gutter_text", "gutter_background"),
    (ColorGroup::Text, "syntax_plain", "same_line"),
    (ColorGroup::Text, "syntax_comment", "same_line"),
    (ColorGroup::Text, "syntax_literal", "same_line"),
    (ColorGroup::Text, "syntax_number", "same_line"),
    (ColorGroup::Text, "syntax_keyword", "same_line"),
    (ColorGroup::Text, "syntax_identifier", "same_line"),
    (ColorGroup::Text, "syntax_directive", "same_line"),
    (ColorGroup::Text, "syntax_operator", "same_line"),
    (ColorGroup::Text, "syntax_tag", "same_line"),
    (ColorGroup::Text, "syntax_attribute", "same_line"),
    (ColorGroup::Text, "syntax_section", "same_line"),
    (ColorGroup::Text, "syntax_block", "same_line"),
    (ColorGroup::Text, "syntax_other", "same_line"),
    (ColorGroup::Text, "settings_overridden", "chrome"),
    (ColorGroup::Text, "settings_inherited", "chrome"),
    (ColorGroup::Text, "notice_info", "chrome"),
    (ColorGroup::Text, "notice_warning", "chrome"),
    (ColorGroup::Text, "notice_error", "chrome"),
    (ColorGroup::Folder, "folder_same", "folder_background"),
    (ColorGroup::Folder, "folder_different", "folder_background"),
    (ColorGroup::Folder, "folder_orphan", "folder_background"),
    (ColorGroup::Folder, "folder_error", "folder_background"),
    (ColorGroup::Folder, "folder_unknown", "folder_background"),
    (ColorGroup::Folder, "folder_same", "stripe"),
    (ColorGroup::Folder, "folder_same", "folder_selection"),
    (ColorGroup::Folder, "folder_different", "folder_selection"),
    (
        ColorGroup::Folder,
        "folder_header_text",
        "folder_header_background",
    ),
    (ColorGroup::Folder, "folder_same", "center_column"),
    (ColorGroup::Hex, "same_text", "pane_focused"),
    (ColorGroup::Hex, "same_text", "pane_other"),
    (ColorGroup::Hex, "different_text", "different_background"),
    (ColorGroup::Hex, "orphan_text", "orphan_background"),
    (ColorGroup::Hex, "address_text", "address_background"),
    (ColorGroup::Hex, "same_text", "gap_background"),
    (ColorGroup::Hex, "same_text", "selection"),
    (ColorGroup::Table, "same_text", "same_background"),
    (ColorGroup::Table, "same_text", "stripe"),
    (ColorGroup::Table, "same_text", "selection"),
    (ColorGroup::Table, "same_text", "gap_background"),
    (ColorGroup::Table, "header_text", "header_background"),
    (ColorGroup::Table, "header_text", "header_hover"),
    (ColorGroup::Table, "gutter_text", "gutter_background"),
    (ColorGroup::Table, "different_text", "different_background"),
    (
        ColorGroup::Table,
        "unimportant_text",
        "unimportant_background",
    ),
    (ColorGroup::Table, "orphan_text", "orphan_background"),
    (ColorGroup::Table, "notice_text", "notice_background"),
    (ColorGroup::Table, "details_text", "details_background"),
];

fn hex(color: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
}

#[test]
fn text_colors_keep_aa_contrast_against_their_backgrounds() {
    let mut failures = Vec::new();
    for builtin in BUILTIN {
        let pack = load::load(builtin.manifest, &memory_files(builtin)).unwrap();
        let dark = pack.modes().dark;
        let variant = if dark { Variant::Dark } else { Variant::Light };
        let stored = pack::layered(pack.colors(), &PaletteOptions::default());
        for (group, text, background) in PAIRS {
            let table = stored.group(*group).table(dark);
            let a = color_of(*group, variant, table, text);
            let b = color_of(*group, variant, table, background);
            let value = ratio(a, b);
            println!(
                "{} {group:?} {text} {} on {background} {} = {value:.2}",
                pack.id(),
                hex(a),
                hex(b)
            );
            if value < AA_NORMAL_TEXT {
                failures.push(format!(
                    "{} {group:?} {text}/{background} {value:.2}",
                    pack.id()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
