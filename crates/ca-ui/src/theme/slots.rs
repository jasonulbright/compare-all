//! The named color slots a person can edit, and the tables they resolve into.
//!
//! Every color the program paints with is still a constant in this module tree.
//! A stored option replaces one of those constants for one variant of one view
//! kind; a slot with no stored value keeps the built-in color.

use super::{hex, merge, picture, table, Palette, Variant};
use ca_session::options::{ColorGroup, ColorTable, PaletteOptions, Rgb};
use egui::Color32;

/// One editable color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Stored name of the slot.
    pub name: &'static str,
    /// Label shown beside the control.
    pub label: &'static str,
    /// Name of the group of related slots the page lists it under.
    pub group: &'static str,
}

/// The color a stored value stands for.
#[must_use]
pub const fn to_color(value: Rgb) -> Color32 {
    Color32::from_rgb(value.red, value.green, value.blue)
}

/// The stored value of a color.
#[must_use]
pub const fn to_stored(color: Color32) -> Rgb {
    Rgb::new(color.r(), color.g(), color.b())
}

/// Declares the slots of one palette type together with the two functions that
/// read and apply them, so a slot cannot be listed without being wired.
macro_rules! palette_slots {
    (
        $slots:ident, $apply:ident, $read:ident, $palette:ty,
        $($name:literal => $($field:ident).+, $group:literal, $label:literal;)*
    ) => {
        /// Every editable color of this palette, in page order.
        pub const $slots: &[Slot] = &[
            $(Slot { name: $name, label: $label, group: $group },)*
        ];

        /// The palette with every stored color applied over the built-in one.
        #[must_use]
        pub fn $apply(mut base: $palette, stored: &ColorTable) -> $palette {
            $(
                if let Some(color) = stored.get($name) {
                    base.$($field).+ = to_color(color);
                }
            )*
            base
        }

        /// The color one slot carries in `base`.
        #[must_use]
        pub fn $read(base: &$palette, name: &str) -> Option<Color32> {
            match name {
                $($name => Some(base.$($field).+),)*
                _ => None,
            }
        }
    };
}

palette_slots! {
    TEXT_SLOTS, apply_text, read_text, Palette,
    "same_line" => same_line, "Text", "Same text background, focused pane";
    "same_text" => same_text, "Text", "Same text, focused pane";
    "same_line_other" => same_line_other, "Text", "Same text background, other pane";
    "same_text_other" => same_text_other, "Text", "Same text, other pane";
    "important_line" => important_line, "Text", "Important difference background";
    "important_text" => important_text, "Text", "Important difference text";
    "unimportant_line" => unimportant_line, "Text", "Unimportant difference background, focused pane";
    "unimportant_text" => unimportant_text, "Text", "Unimportant difference text, focused pane";
    "unimportant_line_other" => unimportant_line_other, "Text", "Unimportant difference background, other pane";
    "unimportant_text_other" => unimportant_text_other, "Text", "Unimportant difference text, other pane";
    "orphan_line" => orphan_line, "Text", "Orphan background";
    "orphan_text" => orphan_text, "Text", "Orphan text";
    "gap_pattern" => gap_pattern, "Text", "Filler row pattern";
    "caret_line" => caret_line, "Text", "Caret row background";
    "stripe" => stripe, "Text", "Alternating row tint";
    "selection" => selection, "Text", "Selected row background";
    "gutter_background" => gutter_background, "Chrome", "Gutter background";
    "gutter_text" => gutter_text, "Chrome", "Gutter text";
    "gutter_arrow" => gutter_arrow, "Chrome", "Gutter copy arrow";
    "chrome" => chrome, "Chrome", "Status bar and detail area";
    "toolbar" => toolbar, "Chrome", "Toolbar background";
    "separator" => separator, "Chrome", "Pane separator";
    "separator_strong" => separator_strong, "Chrome", "Pane separator, stronger line";
    "thumbnail_background" => thumbnail_background, "Chrome", "Overview strip background";
    "thumbnail_marker" => thumbnail_marker, "Chrome", "Overview strip viewport marker";
    "thumbnail_caret" => thumbnail_caret, "Chrome", "Overview strip caret marker";
    "settings_overridden" => settings_overridden, "Chrome", "Settings label, stated value";
    "settings_inherited" => settings_inherited, "Chrome", "Settings label, inherited value";
    "notice_info" => notice_info, "Chrome", "Notice, information";
    "notice_warning" => notice_warning, "Chrome", "Notice, warning";
    "notice_error" => notice_error, "Chrome", "Notice, failure";
    "syntax_plain" => syntax.plain, "Syntax", "Plain text";
    "syntax_whitespace" => syntax.whitespace, "Syntax", "Whitespace";
    "syntax_comment" => syntax.comment, "Syntax", "Comment";
    "syntax_literal" => syntax.literal, "Syntax", "String literal";
    "syntax_number" => syntax.number, "Syntax", "Number";
    "syntax_keyword" => syntax.keyword, "Syntax", "Reserved word";
    "syntax_identifier" => syntax.identifier, "Syntax", "Name";
    "syntax_directive" => syntax.directive, "Syntax", "Directive";
    "syntax_operator" => syntax.operator, "Syntax", "Operator";
    "syntax_tag" => syntax.tag, "Syntax", "Markup tag";
    "syntax_attribute" => syntax.attribute, "Syntax", "Attribute";
    "syntax_section" => syntax.section, "Syntax", "Section";
    "syntax_block" => syntax.block, "Syntax", "Block";
    "syntax_other" => syntax.other, "Syntax", "Other element";
}

palette_slots! {
    FOLDER_SLOTS, apply_folder, read_folder, Palette,
    "folder_background" => folder_background, "Rows", "Row background";
    "stripe" => stripe, "Rows", "Alternating row tint";
    "folder_selection" => folder_selection, "Rows", "Selected row background";
    "folder_selection_edge" => folder_selection_edge, "Rows", "Selected row edge";
    "folder_header_background" => folder_header_background, "Rows", "Column header background";
    "folder_header_text" => folder_header_text, "Rows", "Column header text";
    "center_column" => center_column, "Rows", "Center status column";
    "folder_same" => folder_same, "Comparison", "Matching item";
    "folder_different" => folder_different, "Comparison", "Differing item";
    "folder_orphan" => folder_orphan, "Comparison", "Item on one side only";
    "folder_error" => folder_error, "Comparison", "Item that could not be compared";
    "folder_unknown" => folder_unknown, "Comparison", "Item not compared yet";
}

palette_slots! {
    HEX_SLOTS, apply_hex, read_hex, hex::Palette,
    "background" => background, "Panes", "View background";
    "pane_focused" => pane_focused, "Panes", "Focused pane background";
    "pane_other" => pane_other, "Panes", "Other pane background";
    "same_text" => same_text, "Bytes", "Matching byte";
    "different_text" => different_text, "Bytes", "Differing byte";
    "different_background" => different_background, "Bytes", "Differing byte background";
    "orphan_text" => orphan_text, "Bytes", "Byte on one side only";
    "orphan_background" => orphan_background, "Bytes", "Byte on one side only, background";
    "gap_background" => gap_background, "Bytes", "Filler run background";
    "gap_pattern" => gap_pattern, "Bytes", "Filler run pattern";
    "unavailable_text" => unavailable_text, "Bytes", "Byte not held in memory";
    "address_text" => address_text, "Chrome", "Address column text";
    "address_background" => address_background, "Chrome", "Address column background";
    "separator" => separator, "Chrome", "Area separator";
    "caret" => caret, "Chrome", "Caret";
    "selection" => selection, "Chrome", "Selected bytes";
    "thumbnail_background" => thumbnail_background, "Chrome", "Overview strip background";
    "thumbnail_marker" => thumbnail_marker, "Chrome", "Overview strip viewport marker";
    "thumbnail_caret" => thumbnail_caret, "Chrome", "Overview strip caret marker";
}

palette_slots! {
    TABLE_SLOTS, apply_table, read_table, table::Palette,
    "background" => background, "Grid", "View background";
    "panel" => panel, "Grid", "Panel background";
    "grid_line" => grid_line, "Grid", "Grid line";
    "stripe" => stripe, "Grid", "Alternating row tint";
    "header_background" => header_background, "Grid", "Column header background";
    "header_text" => header_text, "Grid", "Column header text";
    "header_hover" => header_hover, "Grid", "Column header under the pointer";
    "gutter_background" => gutter_background, "Grid", "Row header background";
    "gutter_text" => gutter_text, "Grid", "Row header text";
    "separator" => separator, "Grid", "Panel separator";
    "same_background" => same_background, "Cells", "Matching cell background";
    "same_text" => same_text, "Cells", "Matching cell text";
    "different_background" => different_background, "Cells", "Differing cell background";
    "different_text" => different_text, "Cells", "Differing cell text";
    "unimportant_background" => unimportant_background, "Cells", "Unimportant cell background";
    "unimportant_text" => unimportant_text, "Cells", "Unimportant cell text";
    "orphan_background" => orphan_background, "Cells", "Cell on one side only, background";
    "orphan_text" => orphan_text, "Cells", "Cell on one side only, text";
    "gap_background" => gap_background, "Cells", "Filler cell background";
    "current_cell_border" => current_cell_border, "Cells", "Current cell outline";
    "selection" => selection, "Cells", "Selected cell background";
    "key_marker" => key_marker, "Markers", "Key column marker";
    "unimportant_marker" => unimportant_marker, "Markers", "Unimportant column marker";
    "spot_important" => spot_important, "Markers", "Overview mark, important";
    "spot_unimportant" => spot_unimportant, "Markers", "Overview mark, unimportant";
    "spot_orphan" => spot_orphan, "Markers", "Overview mark, one side only";
    "thumbnail_background" => thumbnail_background, "Markers", "Overview strip background";
    "thumbnail_marker" => thumbnail_marker, "Markers", "Overview strip viewport marker";
    "notice_background" => notice_background, "Chrome", "Notice background";
    "notice_text" => notice_text, "Chrome", "Notice text";
    "details_background" => details_background, "Chrome", "Details background";
    "details_text" => details_text, "Chrome", "Details text";
}

palette_slots! {
    PICTURE_SLOTS, apply_picture, read_picture, picture::Palette,
    "background" => background, "Panes", "View background";
    "pane_background" => pane_background, "Panes", "Image pane background";
    "pane_border" => pane_border, "Panes", "Pane outline";
    "pane_border_focused" => pane_border_focused, "Panes", "Focused pane outline";
    "panel_background" => panel_background, "Chrome", "Control column background";
    "label_text" => label_text, "Chrome", "Field name";
    "value_text" => value_text, "Chrome", "Field value";
    "error_text" => error_text, "Chrome", "Failure message";
    "notice_text" => notice_text, "Chrome", "Notice message";
    "progress_text" => progress_text, "Chrome", "Progress message";
    "crosshair" => crosshair, "Chrome", "Crosshair";
    "crosshair_echo" => crosshair_echo, "Chrome", "Crosshair in the other panes";
    "separator" => separator, "Chrome", "Panel separator";
}

palette_slots! {
    MERGE_SLOTS, apply_merge, read_merge, merge::Palette,
    "unchanged_line" => unchanged_line, "Rows", "Unchanged row background";
    "unchanged_text" => unchanged_text, "Rows", "Unchanged row text";
    "left_line" => left_line, "Rows", "Left change background";
    "left_text" => left_text, "Rows", "Left change text";
    "right_line" => right_line, "Rows", "Right change background";
    "right_text" => right_text, "Rows", "Right change text";
    "same_change_line" => same_change_line, "Rows", "Same change background";
    "same_change_text" => same_change_text, "Rows", "Same change text";
    "conflict_line" => conflict_line, "Rows", "Conflict background";
    "conflict_text" => conflict_text, "Rows", "Conflict text";
    "edited_line" => edited_line, "Rows", "Edited output background";
    "edited_text" => edited_text, "Rows", "Edited output text";
    "gap_line" => gap_line, "Rows", "Filler row background";
    "gap_pattern" => gap_pattern, "Rows", "Filler row pattern";
}

/// The editable slots of one color group.
#[must_use]
pub const fn slots_of(group: ColorGroup) -> &'static [Slot] {
    match group {
        ColorGroup::Text => TEXT_SLOTS,
        ColorGroup::Folder => FOLDER_SLOTS,
        ColorGroup::Hex => HEX_SLOTS,
        ColorGroup::Table => TABLE_SLOTS,
        ColorGroup::Picture => PICTURE_SLOTS,
        ColorGroup::Merge => MERGE_SLOTS,
    }
}

/// The color one slot of one group carries, before and after the stored values
/// are applied.
#[must_use]
pub fn color_of(group: ColorGroup, variant: Variant, stored: &ColorTable, name: &str) -> Color32 {
    let dark = matches!(variant, Variant::Dark);
    match group {
        ColorGroup::Text => read_text(&apply_text(super::palette(variant), stored), name),
        ColorGroup::Folder => read_folder(&apply_folder(super::palette(variant), stored), name),
        ColorGroup::Hex => read_hex(&apply_hex(*hex::palette(variant), stored), name),
        ColorGroup::Table => read_table(&apply_table(*table::palette(variant), stored), name),
        ColorGroup::Picture => {
            read_picture(&apply_picture(*picture::palette(variant), stored), name)
        }
        ColorGroup::Merge => read_merge(&apply_merge(merge::palette(variant), stored), name),
    }
    .unwrap_or(if dark {
        Color32::from_rgb(0, 0, 0)
    } else {
        Color32::from_rgb(0xFF, 0xFF, 0xFF)
    })
}

/// The whole set of tables one options document resolves to.
#[derive(Debug, Clone, Copy)]
pub struct Tables {
    /// Which variant these tables are.
    pub variant: Variant,
    /// The shared table the text and folder views paint with.
    pub main: Palette,
    /// The byte comparison table.
    pub hex: hex::Palette,
    /// The table comparison table.
    pub table: table::Palette,
    /// The picture comparison table.
    pub picture: picture::Palette,
    /// The three way merge table.
    pub merge: merge::Palette,
}

impl Tables {
    /// The built-in tables of one variant with every stored color applied.
    ///
    /// The text group and the folder group both write into the shared table,
    /// which is why they are applied one after the other rather than into two
    /// tables that would have to be kept in step.
    #[must_use]
    pub fn resolve(variant: Variant, options: &PaletteOptions) -> Self {
        let dark = matches!(variant, Variant::Dark);
        let main = apply_folder(
            apply_text(super::palette(variant), options.text.table(dark)),
            options.folder.table(dark),
        );
        Self {
            variant,
            main,
            hex: apply_hex(*hex::palette(variant), options.hex.table(dark)),
            table: apply_table(*table::palette(variant), options.table.table(dark)),
            picture: apply_picture(*picture::palette(variant), options.picture.table(dark)),
            merge: apply_merge(merge::palette(variant), options.merge.table(dark)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{color_of, slots_of, to_stored, Tables, TEXT_SLOTS};
    use crate::theme::Variant;
    use ca_session::options::{ColorGroup, ColorTable, PaletteOptions, Rgb};

    #[test]
    fn no_group_lists_the_same_slot_twice() {
        for group in ColorGroup::ALL {
            let slots = slots_of(*group);
            for (index, slot) in slots.iter().enumerate() {
                for other in slots.iter().skip(index + 1) {
                    assert_ne!(slot.name, other.name, "{} in {}", slot.name, group.id());
                }
            }
            assert!(!slots.is_empty(), "{} has no slots", group.id());
        }
    }

    #[test]
    fn every_slot_of_every_group_reads_a_color_in_both_variants() {
        let empty = ColorTable::default();
        for group in ColorGroup::ALL {
            for slot in slots_of(*group) {
                for variant in [Variant::Light, Variant::Dark] {
                    let built_in = color_of(*group, variant, &empty, slot.name);
                    let mut stored = ColorTable::default();
                    stored.set(slot.name, Rgb::new(0x12, 0x34, 0x56));
                    assert_eq!(
                        to_stored(color_of(*group, variant, &stored, slot.name)),
                        Rgb::new(0x12, 0x34, 0x56),
                        "{} {} does not take a stored value",
                        group.id(),
                        slot.name
                    );
                    let _ = built_in;
                }
            }
        }
    }

    #[test]
    fn a_table_with_nothing_stored_is_the_built_in_table() {
        let tables = Tables::resolve(Variant::Dark, &PaletteOptions::default());
        assert_eq!(tables.main.same_line, crate::theme::DARK.same_line);
        assert_eq!(
            tables.merge.conflict_line,
            crate::theme::merge::DARK.conflict_line
        );
    }

    #[test]
    fn a_stored_color_reaches_the_resolved_table_of_its_variant_only() {
        let mut options = PaletteOptions::default();
        options
            .text
            .table_mut(true)
            .set("same_line", Rgb::new(0x01, 0x02, 0x03));
        let dark = Tables::resolve(Variant::Dark, &options);
        let light = Tables::resolve(Variant::Light, &options);
        assert_eq!(to_stored(dark.main.same_line), Rgb::new(0x01, 0x02, 0x03));
        assert_eq!(light.main.same_line, crate::theme::LIGHT.same_line);
    }

    #[test]
    fn the_folder_group_and_the_text_group_both_reach_the_shared_table() {
        let mut options = PaletteOptions::default();
        options
            .folder
            .table_mut(true)
            .set("folder_different", Rgb::new(9, 9, 9));
        options
            .text
            .table_mut(true)
            .set("important_text", Rgb::new(8, 8, 8));
        let tables = Tables::resolve(Variant::Dark, &options);
        assert_eq!(to_stored(tables.main.folder_different), Rgb::new(9, 9, 9));
        assert_eq!(to_stored(tables.main.important_text), Rgb::new(8, 8, 8));
    }

    #[test]
    fn the_text_group_covers_every_syntax_element() {
        let syntax = TEXT_SLOTS
            .iter()
            .filter(|slot| slot.group == "Syntax")
            .count();
        assert_eq!(syntax, 14);
    }
}
