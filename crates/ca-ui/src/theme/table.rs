//! Colors of the table comparison view, for both theme variants.

use super::Variant;
use egui::Color32;

/// What a grid cell is painted as.
///
/// The classes stand on their own rather than naming an engine type, because
/// this crate never depends on a comparison engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellClass {
    /// Both sides hold the same value.
    Same,
    /// The two sides differ in a way that matters.
    Different,
    /// The two sides differ in a way that does not matter.
    Unimportant,
    /// The cell exists on the left side only.
    LeftOnly,
    /// The cell exists on the right side only.
    RightOnly,
    /// Filler that holds a side in alignment against a row it does not have.
    Gap,
}

/// Colors the table comparison view paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// View background.
    pub background: Color32,
    /// Background of the bars above and below the grids.
    pub panel: Color32,
    /// Line drawn between the two grids and around the panels.
    pub separator: Color32,
    /// Column header background.
    pub header_background: Color32,
    /// Column header text.
    pub header_text: Color32,
    /// Background of a column header the pointer rests on.
    pub header_hover: Color32,
    /// Row number gutter background.
    pub gutter_background: Color32,
    /// Row number gutter text.
    pub gutter_text: Color32,
    /// Line drawn between cells.
    pub grid_line: Color32,
    /// Tint applied to alternating rows.
    pub stripe: Color32,
    /// Background of a cell whose two sides match.
    pub same_background: Color32,
    /// Text of a cell whose two sides match.
    pub same_text: Color32,
    /// Background of a cell holding a difference that matters.
    pub different_background: Color32,
    /// Text of a cell holding a difference that matters.
    pub different_text: Color32,
    /// Background of a cell holding a difference that does not matter.
    pub unimportant_background: Color32,
    /// Text of a cell holding a difference that does not matter.
    pub unimportant_text: Color32,
    /// Background of a cell present on one side only.
    pub orphan_background: Color32,
    /// Text of a cell present on one side only.
    pub orphan_text: Color32,
    /// Background of filler rows that hold a side in alignment.
    pub gap_background: Color32,
    /// Border drawn around the current cell.
    pub current_cell_border: Color32,
    /// Background of the row the current cell sits on.
    pub selection: Color32,
    /// Mark drawn on the header of a key column.
    pub key_marker: Color32,
    /// Mark drawn on the header of a column set unimportant.
    pub unimportant_marker: Color32,
    /// Gutter spot for a row holding a difference that matters.
    pub spot_important: Color32,
    /// Gutter spot for a row holding a difference that does not matter.
    pub spot_unimportant: Color32,
    /// Gutter spot for a row present on one side only.
    pub spot_orphan: Color32,
    /// Background of the thumbnail strip.
    pub thumbnail_background: Color32,
    /// Outline of the thumbnail's viewport marker.
    pub thumbnail_marker: Color32,
    /// Background of the notice bar.
    pub notice_background: Color32,
    /// Text of the notice bar.
    pub notice_text: Color32,
    /// Background of the cell details area.
    pub details_background: Color32,
    /// Text of the cell details area.
    pub details_text: Color32,
}

impl Palette {
    /// Background and text color for a cell class.
    #[must_use]
    pub const fn cell(&self, class: CellClass) -> (Color32, Color32) {
        match class {
            CellClass::Same => (self.same_background, self.same_text),
            CellClass::Different => (self.different_background, self.different_text),
            CellClass::Unimportant => (self.unimportant_background, self.unimportant_text),
            CellClass::LeftOnly | CellClass::RightOnly => {
                (self.orphan_background, self.orphan_text)
            }
            CellClass::Gap => (self.gap_background, self.same_text),
        }
    }

    /// The gutter spot for a row class, where the class has one.
    #[must_use]
    pub const fn spot(&self, class: CellClass) -> Option<Color32> {
        match class {
            CellClass::Same | CellClass::Gap => None,
            CellClass::Different => Some(self.spot_important),
            CellClass::Unimportant => Some(self.spot_unimportant),
            CellClass::LeftOnly | CellClass::RightOnly => Some(self.spot_orphan),
        }
    }
}

/// Light variant.
pub const LIGHT: Palette = Palette {
    background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    panel: Color32::from_rgb(0xF0, 0xF0, 0xF0),
    separator: Color32::from_rgb(0xB4, 0xB4, 0xB4),
    header_background: Color32::from_rgb(0xE6, 0xE6, 0xE6),
    header_text: Color32::from_rgb(0x10, 0x10, 0x10),
    header_hover: Color32::from_rgb(0xD4, 0xD4, 0xD4),
    gutter_background: Color32::from_rgb(0xEE, 0xEE, 0xEE),
    gutter_text: Color32::from_rgb(0x6C, 0x6C, 0x6C),
    grid_line: Color32::from_rgb(0xD8, 0xD8, 0xD8),
    stripe: Color32::from_rgb(0xF4, 0xF4, 0xF4),
    same_background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    same_text: Color32::from_rgb(0x10, 0x10, 0x10),
    different_background: Color32::from_rgb(0xFF, 0xE4, 0xE4),
    different_text: Color32::from_rgb(0xC0, 0x00, 0x00),
    unimportant_background: Color32::from_rgb(0xE4, 0xE8, 0xFF),
    unimportant_text: Color32::from_rgb(0x00, 0x00, 0xC0),
    orphan_background: Color32::from_rgb(0xF6, 0xE6, 0xF6),
    orphan_text: Color32::from_rgb(0x80, 0x00, 0x80),
    gap_background: Color32::from_rgb(0xE8, 0xE8, 0xE8),
    current_cell_border: Color32::from_rgb(0x00, 0x60, 0xC0),
    selection: Color32::from_rgb(0xCC, 0xDE, 0xF6),
    key_marker: Color32::from_rgb(0xAC, 0x7A, 0x00),
    unimportant_marker: Color32::from_rgb(0x00, 0x00, 0xC0),
    spot_important: Color32::from_rgb(0xC0, 0x00, 0x00),
    spot_unimportant: Color32::from_rgb(0x00, 0x00, 0xC0),
    spot_orphan: Color32::from_rgb(0x80, 0x00, 0x80),
    thumbnail_background: Color32::from_rgb(0xE0, 0xE0, 0xE0),
    thumbnail_marker: Color32::from_rgb(0x20, 0x20, 0x20),
    notice_background: Color32::from_rgb(0xFF, 0xF4, 0xD0),
    notice_text: Color32::from_rgb(0x50, 0x38, 0x00),
    details_background: Color32::from_rgb(0xF6, 0xF6, 0xF6),
    details_text: Color32::from_rgb(0x10, 0x10, 0x10),
};

/// Dark variant.
pub const DARK: Palette = Palette {
    background: Color32::from_rgb(0x1B, 0x1E, 0x24),
    panel: Color32::from_rgb(0x25, 0x29, 0x2F),
    separator: Color32::from_rgb(0x39, 0x3D, 0x44),
    header_background: Color32::from_rgb(0x2E, 0x32, 0x38),
    header_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    header_hover: Color32::from_rgb(0x48, 0x4C, 0x52),
    gutter_background: Color32::from_rgb(0x21, 0x25, 0x2A),
    gutter_text: Color32::from_rgb(0x99, 0x9D, 0xA5),
    grid_line: Color32::from_rgb(0x39, 0x3D, 0x44),
    stripe: Color32::from_rgb(0x20, 0x24, 0x29),
    same_background: Color32::from_rgb(0x1B, 0x1E, 0x24),
    same_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    different_background: Color32::from_rgb(0x4A, 0x23, 0x23),
    different_text: Color32::from_rgb(0xFB, 0x9F, 0x88),
    unimportant_background: Color32::from_rgb(0x20, 0x30, 0x3D),
    unimportant_text: Color32::from_rgb(0x94, 0xAF, 0xC3),
    orphan_background: Color32::from_rgb(0x35, 0x29, 0x49),
    orphan_text: Color32::from_rgb(0xCA, 0xAC, 0xFF),
    gap_background: Color32::from_rgb(0x21, 0x25, 0x2A),
    current_cell_border: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    selection: Color32::from_rgb(0x18, 0x33, 0x56),
    key_marker: Color32::from_rgb(0xF2, 0xCD, 0x6F),
    unimportant_marker: Color32::from_rgb(0x94, 0xAF, 0xC3),
    spot_important: Color32::from_rgb(0xFB, 0x9F, 0x88),
    spot_unimportant: Color32::from_rgb(0x94, 0xAF, 0xC3),
    spot_orphan: Color32::from_rgb(0xCA, 0xAC, 0xFF),
    thumbnail_background: Color32::from_rgb(0x25, 0x29, 0x2F),
    thumbnail_marker: Color32::from_rgb(0xCD, 0xD1, 0xD8),
    notice_background: Color32::from_rgb(0x3B, 0x32, 0x1A),
    notice_text: Color32::from_rgb(0xE9, 0xD3, 0x97),
    details_background: Color32::from_rgb(0x25, 0x29, 0x2F),
    details_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
};

/// The palette for a theme variant.
#[must_use]
pub const fn palette(variant: Variant) -> &'static Palette {
    match variant {
        Variant::Light => &LIGHT,
        Variant::Dark => &DARK,
    }
}

impl crate::thumbnail::Severity for CellClass {
    fn rank(self) -> u8 {
        match self {
            CellClass::Same | CellClass::Gap => 0,
            CellClass::Unimportant => 1,
            CellClass::LeftOnly | CellClass::RightOnly => 2,
            CellClass::Different => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{palette, CellClass, DARK, LIGHT};
    use crate::theme::Variant;

    const CLASSES: [CellClass; 6] = [
        CellClass::Same,
        CellClass::Different,
        CellClass::Unimportant,
        CellClass::LeftOnly,
        CellClass::RightOnly,
        CellClass::Gap,
    ];

    #[test]
    fn every_class_is_readable_in_both_variants() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for class in CLASSES {
                let (background, text) = table.cell(class);
                assert_ne!(background, text, "{variant:?} {class:?} is unreadable");
            }
        }
    }

    #[test]
    fn matching_rows_carry_no_gutter_spot() {
        assert!(LIGHT.spot(CellClass::Same).is_none());
        assert!(DARK.spot(CellClass::Gap).is_none());
        assert!(DARK.spot(CellClass::Different).is_some());
    }

    #[test]
    fn the_two_variants_differ() {
        assert_ne!(LIGHT.background, DARK.background);
        assert_ne!(LIGHT.different_background, DARK.different_background);
    }
}
