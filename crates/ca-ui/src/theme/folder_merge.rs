//! Colors of the folder merge view, for both theme variants.
//!
//! The legend names six text colors, one for each class.

use super::Variant;
use egui::Color32;

/// What a folder merge row is painted as.
///
/// The classes stand on their own rather than naming an engine type, because
/// the theme maps classes to colors and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderMergeClass {
    /// No status can be proved, or the item is older.
    Unknown,
    /// Neither side changed the item.
    Same,
    /// Only the left side changed the item.
    LeftChange,
    /// Only the right side changed the item.
    RightChange,
    /// Both sides changed the item and a text merge resolves it.
    Mergeable,
    /// Both sides changed the item differently.
    Conflict,
}

impl FolderMergeClass {
    /// Every class, in the order a legend lists them.
    pub const ALL: [Self; 6] = [
        Self::Unknown,
        Self::Same,
        Self::LeftChange,
        Self::RightChange,
        Self::Mergeable,
        Self::Conflict,
    ];

    /// The name a legend shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown or older",
            Self::Same => "Same",
            Self::LeftChange => "Left change",
            Self::RightChange => "Right change",
            Self::Mergeable => "Mergeable",
            Self::Conflict => "Conflict",
        }
    }
}

/// Colors the folder merge view paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Text of an item with no provable status.
    pub unknown: Color32,
    /// Text of an unchanged item.
    pub same: Color32,
    /// Text of an item only the left side changed.
    pub left: Color32,
    /// Text of an item only the right side changed.
    pub right: Color32,
    /// Text of an item a text merge resolves.
    pub mergeable: Color32,
    /// Text of an item both sides changed differently.
    pub conflict: Color32,
    /// Background of the output column.
    pub output_column: Color32,
}

impl Palette {
    /// The text color of one class.
    #[must_use]
    pub const fn text(&self, class: FolderMergeClass) -> Color32 {
        match class {
            FolderMergeClass::Unknown => self.unknown,
            FolderMergeClass::Same => self.same,
            FolderMergeClass::LeftChange => self.left,
            FolderMergeClass::RightChange => self.right,
            FolderMergeClass::Mergeable => self.mergeable,
            FolderMergeClass::Conflict => self.conflict,
        }
    }
}

/// The light table. Provisional.
pub const LIGHT: Palette = Palette {
    unknown: Color32::from_rgb(0x61, 0x61, 0x61),
    same: Color32::from_rgb(0x10, 0x10, 0x10),
    left: Color32::from_rgb(0x00, 0x6D, 0x6D),
    right: Color32::from_rgb(0xAD, 0x00, 0xAD),
    mergeable: Color32::from_rgb(0xA1, 0x45, 0x00),
    conflict: Color32::from_rgb(0xC0, 0x00, 0x00),
    output_column: Color32::from_rgb(0xF4, 0xF4, 0xF4),
};

/// The dark table.
pub const DARK: Palette = Palette {
    unknown: Color32::from_rgb(0xA2, 0xA5, 0xAA),
    same: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    left: Color32::from_rgb(0x67, 0xAD, 0xDD),
    right: Color32::from_rgb(0xF2, 0xB7, 0x72),
    mergeable: Color32::from_rgb(0x97, 0xCC, 0xAE),
    conflict: Color32::from_rgb(0xFF, 0x88, 0x7A),
    output_column: Color32::from_rgb(0x20, 0x24, 0x29),
};

/// The folder merge table for `variant`.
#[must_use]
pub const fn palette(variant: Variant) -> Palette {
    match variant {
        Variant::Light => LIGHT,
        Variant::Dark => DARK,
    }
}

#[cfg(test)]
mod tests {
    use super::{palette, FolderMergeClass};
    use crate::theme::Variant;

    #[test]
    fn every_class_has_its_own_color_in_both_tables() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for (index, class) in FolderMergeClass::ALL.iter().enumerate() {
                assert!(!class.label().is_empty());
                for other in FolderMergeClass::ALL.iter().skip(index + 1) {
                    assert_ne!(
                        table.text(*class),
                        table.text(*other),
                        "{} and {} share a color in {variant:?}",
                        class.label(),
                        other.label()
                    );
                }
            }
        }
    }
}
