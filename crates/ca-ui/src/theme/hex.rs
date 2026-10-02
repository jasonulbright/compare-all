//! Colors of the hex comparison view, for both theme variants.

use super::Variant;
use egui::Color32;

/// How one byte is classified against its counterpart on the other side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteClass {
    /// Both sides carry the byte and the two agree.
    Same,
    /// Both sides carry the byte and the two differ.
    Different,
    /// Only the left side carries the byte.
    LeftOnly,
    /// Only the right side carries the byte.
    RightOnly,
    /// Filler standing in for a byte the other side has and this one does not.
    Gap,
}

/// Colors the hex comparison view paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// View background.
    pub background: Color32,
    /// Background of the pane the caret is in.
    pub pane_focused: Color32,
    /// Background of the pane the caret is not in.
    pub pane_other: Color32,
    /// Byte and character text where both sides agree.
    pub same_text: Color32,
    /// Byte and character text where the two sides differ.
    pub different_text: Color32,
    /// Background behind a byte where the two sides differ.
    pub different_background: Color32,
    /// Byte and character text of a byte the other side does not have.
    pub orphan_text: Color32,
    /// Background behind a byte the other side does not have.
    pub orphan_background: Color32,
    /// Background of a filler run that holds a side in alignment.
    pub gap_background: Color32,
    /// Line color of the pattern drawn across a filler run.
    pub gap_pattern: Color32,
    /// Byte address column text.
    pub address_text: Color32,
    /// Byte address column background.
    pub address_background: Color32,
    /// Rule between the address, byte and character areas.
    pub separator: Color32,
    /// Caret line.
    pub caret: Color32,
    /// Background behind the selected bytes.
    pub selection: Color32,
    /// Background of the overview strip.
    pub thumbnail_background: Color32,
    /// Outline of the overview strip's viewport marker.
    pub thumbnail_marker: Color32,
    /// Mark drawn at the caret's position in the overview strip.
    pub thumbnail_caret: Color32,
    /// Text of a byte whose content is not held in memory.
    pub unavailable_text: Color32,
}

/// Light variant.
pub const LIGHT: Palette = Palette {
    background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    pane_focused: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    pane_other: Color32::from_rgb(0xF7, 0xF7, 0xF7),
    same_text: Color32::from_rgb(0x10, 0x10, 0x10),
    different_text: Color32::from_rgb(0xC0, 0x00, 0x00),
    different_background: Color32::from_rgb(0xFF, 0xE4, 0xE4),
    orphan_text: Color32::from_rgb(0x80, 0x00, 0x80),
    orphan_background: Color32::from_rgb(0xF6, 0xE6, 0xF6),
    gap_background: Color32::from_rgb(0xEE, 0xEE, 0xEE),
    gap_pattern: Color32::from_rgb(0xBB, 0xBB, 0xBB),
    address_text: Color32::from_rgb(0x50, 0x50, 0x50),
    address_background: Color32::from_rgb(0xEE, 0xEE, 0xEE),
    separator: Color32::from_rgb(0xC8, 0xC8, 0xC8),
    caret: Color32::from_rgb(0x00, 0x60, 0xC0),
    selection: Color32::from_rgb(0xCC, 0xDE, 0xF6),
    thumbnail_background: Color32::from_rgb(0xE0, 0xE0, 0xE0),
    thumbnail_marker: Color32::from_rgb(0x20, 0x20, 0x20),
    thumbnail_caret: Color32::from_rgb(0x00, 0x60, 0xC0),
    unavailable_text: Color32::from_rgb(0x8F, 0x8F, 0x8F),
};

/// Dark variant.
pub const DARK: Palette = Palette {
    background: Color32::from_rgb(0x1B, 0x1E, 0x24),
    pane_focused: Color32::from_rgb(0x1B, 0x1E, 0x24),
    pane_other: Color32::from_rgb(0x1E, 0x22, 0x28),
    same_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    different_text: Color32::from_rgb(0xFB, 0x9F, 0x88),
    different_background: Color32::from_rgb(0x4A, 0x23, 0x23),
    orphan_text: Color32::from_rgb(0xCA, 0xAC, 0xFF),
    orphan_background: Color32::from_rgb(0x35, 0x29, 0x49),
    gap_background: Color32::from_rgb(0x21, 0x25, 0x2A),
    gap_pattern: Color32::from_rgb(0x49, 0x4D, 0x54),
    address_text: Color32::from_rgb(0x99, 0x9D, 0xA5),
    address_background: Color32::from_rgb(0x21, 0x25, 0x2A),
    separator: Color32::from_rgb(0x39, 0x3D, 0x44),
    caret: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    selection: Color32::from_rgb(0x18, 0x33, 0x56),
    thumbnail_background: Color32::from_rgb(0x25, 0x29, 0x2F),
    thumbnail_marker: Color32::from_rgb(0xCD, 0xD1, 0xD8),
    thumbnail_caret: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    unavailable_text: Color32::from_rgb(0x71, 0x75, 0x7A),
};

impl Palette {
    /// Background and text color for one byte.
    ///
    /// A matching byte takes no background of its own, so the pane's own
    /// background shows through; the caller supplies that.
    #[must_use]
    pub const fn byte_colors(&self, class: ByteClass) -> (Option<Color32>, Color32) {
        match class {
            ByteClass::Same => (None, self.same_text),
            ByteClass::Different => (Some(self.different_background), self.different_text),
            ByteClass::LeftOnly | ByteClass::RightOnly => {
                (Some(self.orphan_background), self.orphan_text)
            }
            ByteClass::Gap => (Some(self.gap_background), self.same_text),
        }
    }
}

/// The palette for a theme variant.
#[must_use]
pub const fn palette(variant: Variant) -> &'static Palette {
    match variant {
        Variant::Light => &LIGHT,
        Variant::Dark => &DARK,
    }
}

#[cfg(test)]
mod tests {
    use super::{palette, ByteClass, Variant, DARK, LIGHT};

    #[test]
    fn every_byte_class_stays_readable_in_both_variants() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for class in [
                ByteClass::Same,
                ByteClass::Different,
                ByteClass::LeftOnly,
                ByteClass::RightOnly,
                ByteClass::Gap,
            ] {
                let (background, text) = table.byte_colors(class);
                let background = background.unwrap_or(table.pane_focused);
                assert_ne!(background, text, "{variant:?} {class:?} is unreadable");
            }
        }
    }

    #[test]
    fn the_two_variants_differ() {
        assert_ne!(LIGHT.background, DARK.background);
        assert_ne!(LIGHT.same_text, DARK.same_text);
    }
}
