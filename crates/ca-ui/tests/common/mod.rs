//! Checks shared by the tests over the packs compiled into the program and the
//! packs shipped as folders.

#![allow(clippy::unwrap_used, missing_docs, dead_code)]

use ca_session::options::{ColorGroup, PaletteOptions};
use ca_ui::pack::{self, LoadedPack};
use ca_ui::theme::slots::color_of;
use ca_ui::theme::Variant;
use egui::Color32;

pub const SIZES: [u32; 4] = [16, 24, 32, 48];
pub const AA_NORMAL_TEXT: f64 = 4.5;

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

/// The lowest contrast ratio over every text and background pair of `pack`,
/// the line that describes it, and every pair below the AA minimum.
pub fn contrast(pack: &LoadedPack) -> (f64, String, Vec<String>) {
    let dark = pack.modes().dark;
    let variant = if dark { Variant::Dark } else { Variant::Light };
    let stored = pack::layered(pack.colors(), &PaletteOptions::default());
    let mut lowest = (f64::MAX, String::new());
    let mut failures = Vec::new();
    for (group, text, background) in PAIRS {
        let table = stored.group(*group).table(dark);
        let a = color_of(*group, variant, table, text);
        let b = color_of(*group, variant, table, background);
        let value = ratio(a, b);
        let line = format!(
            "{} {group:?} {text} {} on {background} {} = {value:.2}",
            pack.id(),
            hex(a),
            hex(b)
        );
        println!("{line}");
        if value < lowest.0 {
            lowest = (value, line.clone());
        }
        if value < AA_NORMAL_TEXT {
            failures.push(line);
        }
    }
    (lowest.0, lowest.1, failures)
}
