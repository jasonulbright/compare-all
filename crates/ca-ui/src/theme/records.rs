//! Colors of the registry, version and media comparison views, for both theme
//! variants.

use super::Variant;
use egui::Color32;

/// What a key or a value row is painted as.
///
/// The classes stand on their own rather than naming an engine type, because
/// this crate never depends on a record engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordClass {
    /// Both sides hold the same content.
    Same,
    /// The two sides differ in a way that matters.
    Different,
    /// The two sides differ in a way that does not matter.
    Unimportant,
    /// The row exists on one side only, or a key holds such rows.
    Orphan,
    /// Filler on the side that does not hold the row.
    Gap,
}

/// Colors the record comparison views paint with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// View background.
    pub background: Color32,
    /// Line drawn between the two panes.
    pub separator: Color32,
    /// Column header background.
    pub header_background: Color32,
    /// Column header text.
    pub header_text: Color32,
    /// Background of the scrollbar track.
    pub track: Color32,
    /// Background of the row the cursor sits on.
    pub selection: Color32,
    /// Text of a row whose two sides match.
    pub same_text: Color32,
    /// Background of a row holding a difference that matters.
    pub different_background: Color32,
    /// Text of a row holding a difference that matters.
    pub different_text: Color32,
    /// Background of a row holding a difference that does not matter.
    pub unimportant_background: Color32,
    /// Text of a row holding a difference that does not matter.
    pub unimportant_text: Color32,
    /// Background of a row present on one side only.
    pub orphan_background: Color32,
    /// Text of a row present on one side only.
    pub orphan_text: Color32,
    /// Background of the side that does not hold a row.
    pub gap_background: Color32,
    /// Hatch lines drawn over the side that does not hold a row.
    pub gap_hatch: Color32,
    /// Background of the characters that differ inside a value.
    pub inline_difference: Color32,
    /// Text of the type column.
    pub type_text: Color32,
    /// Background of the thumbnail strip.
    pub thumbnail_background: Color32,
    /// Outline of the thumbnail's viewport marker and the scrollbar thumb.
    pub thumbnail_marker: Color32,
    /// Background of the details area.
    pub details_background: Color32,
    /// Text of the details area.
    pub details_text: Color32,
}

impl Palette {
    /// Background and text color for a row class.
    ///
    /// A matching row keeps the view background.
    #[must_use]
    pub const fn row(&self, class: RecordClass) -> (Color32, Color32) {
        match class {
            RecordClass::Same => (self.background, self.same_text),
            RecordClass::Different => (self.different_background, self.different_text),
            RecordClass::Unimportant => (self.unimportant_background, self.unimportant_text),
            RecordClass::Orphan => (self.orphan_background, self.orphan_text),
            RecordClass::Gap => (self.gap_background, self.same_text),
        }
    }

    /// The color the thumbnail strip paints for a class.
    #[must_use]
    pub const fn strip(&self, class: RecordClass) -> Color32 {
        match class {
            RecordClass::Same | RecordClass::Gap => self.thumbnail_background,
            RecordClass::Different => self.different_text,
            RecordClass::Unimportant => self.unimportant_text,
            RecordClass::Orphan => self.orphan_text,
        }
    }
}

/// Light variant.
pub const LIGHT: Palette = Palette {
    background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    separator: Color32::from_rgb(0xB4, 0xB4, 0xB4),
    header_background: Color32::from_rgb(0xE6, 0xE6, 0xE6),
    header_text: Color32::from_rgb(0x10, 0x10, 0x10),
    track: Color32::from_rgb(0xEE, 0xEE, 0xEE),
    selection: Color32::from_rgb(0xCC, 0xDE, 0xF6),
    same_text: Color32::from_rgb(0x10, 0x10, 0x10),
    different_background: Color32::from_rgb(0xFF, 0xE4, 0xE4),
    different_text: Color32::from_rgb(0xC0, 0x00, 0x00),
    unimportant_background: Color32::from_rgb(0xE4, 0xE8, 0xFF),
    unimportant_text: Color32::from_rgb(0x00, 0x00, 0xC0),
    orphan_background: Color32::from_rgb(0xF6, 0xE6, 0xF6),
    orphan_text: Color32::from_rgb(0x80, 0x00, 0x80),
    gap_background: Color32::from_rgb(0xF0, 0xF0, 0xF0),
    gap_hatch: Color32::from_rgb(0xD8, 0xD8, 0xD8),
    inline_difference: Color32::from_rgb(0xFF, 0xCB, 0xCB),
    type_text: Color32::from_rgb(0x61, 0x61, 0x61),
    thumbnail_background: Color32::from_rgb(0xE0, 0xE0, 0xE0),
    thumbnail_marker: Color32::from_rgb(0x20, 0x20, 0x20),
    details_background: Color32::from_rgb(0xF6, 0xF6, 0xF6),
    details_text: Color32::from_rgb(0x10, 0x10, 0x10),
};

/// Dark variant.
pub const DARK: Palette = Palette {
    background: Color32::from_rgb(0x1B, 0x1E, 0x24),
    separator: Color32::from_rgb(0x39, 0x3D, 0x44),
    header_background: Color32::from_rgb(0x2E, 0x32, 0x38),
    header_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    track: Color32::from_rgb(0x21, 0x25, 0x2A),
    selection: Color32::from_rgb(0x18, 0x33, 0x56),
    same_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    different_background: Color32::from_rgb(0x4A, 0x23, 0x23),
    different_text: Color32::from_rgb(0xFB, 0x9F, 0x88),
    unimportant_background: Color32::from_rgb(0x20, 0x30, 0x3D),
    unimportant_text: Color32::from_rgb(0x94, 0xAF, 0xC3),
    orphan_background: Color32::from_rgb(0x35, 0x29, 0x49),
    orphan_text: Color32::from_rgb(0xCA, 0xAC, 0xFF),
    gap_background: Color32::from_rgb(0x21, 0x25, 0x2A),
    gap_hatch: Color32::from_rgb(0x34, 0x38, 0x3E),
    inline_difference: Color32::from_rgb(0x65, 0x2B, 0x2B),
    type_text: Color32::from_rgb(0x99, 0x9D, 0xA5),
    thumbnail_background: Color32::from_rgb(0x25, 0x29, 0x2F),
    thumbnail_marker: Color32::from_rgb(0xCD, 0xD1, 0xD8),
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

impl crate::thumbnail::Severity for RecordClass {
    fn rank(self) -> u8 {
        match self {
            RecordClass::Same | RecordClass::Gap => 0,
            RecordClass::Unimportant => 1,
            RecordClass::Orphan => 2,
            RecordClass::Different => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{palette, RecordClass, DARK, LIGHT};
    use crate::theme::Variant;
    use crate::thumbnail::Severity;

    const CLASSES: [RecordClass; 5] = [
        RecordClass::Same,
        RecordClass::Different,
        RecordClass::Unimportant,
        RecordClass::Orphan,
        RecordClass::Gap,
    ];

    #[test]
    fn every_class_is_readable_in_both_variants() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for class in CLASSES {
                let (background, text) = table.row(class);
                assert_ne!(background, text, "{variant:?} {class:?} is unreadable");
            }
        }
    }

    #[test]
    fn a_difference_and_an_orphan_paint_differently() {
        for table in [&LIGHT, &DARK] {
            assert_ne!(
                table.row(RecordClass::Different),
                table.row(RecordClass::Orphan)
            );
        }
    }

    #[test]
    fn a_difference_outranks_an_orphan_on_the_strip() {
        assert!(RecordClass::Different.rank() > RecordClass::Orphan.rank());
        assert_eq!(RecordClass::Same.rank(), 0);
    }
}
