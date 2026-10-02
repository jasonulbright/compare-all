//! Colors of the three way merge view, for both theme variants.
//!
//! Each change class carries a hue of its own. A conflict carries the hue of an
//! important difference in the text comparison table.

use super::Variant;
use egui::Color32;

/// How one row of a merge pane is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeClass {
    /// Neither side changed the ancestor here.
    Unchanged,
    /// Only the left side changed here.
    LeftChange,
    /// Only the right side changed here.
    RightChange,
    /// Both sides made the same change here.
    SameChange,
    /// Both sides changed here and the changes differ.
    Conflict,
    /// The output section was typed into.
    Edited,
    /// Filler holding a pane in alignment.
    Gap,
}

impl MergeClass {
    /// Every class, in the order a legend lists them.
    pub const ALL: [Self; 7] = [
        Self::Unchanged,
        Self::LeftChange,
        Self::RightChange,
        Self::SameChange,
        Self::Conflict,
        Self::Edited,
        Self::Gap,
    ];

    /// The name a legend or a status bar shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "Unchanged",
            Self::LeftChange => "Left change",
            Self::RightChange => "Right change",
            Self::SameChange => "Same change",
            Self::Conflict => "Conflict",
            Self::Edited => "Edited",
            Self::Gap => "Gap",
        }
    }

    /// True when the class counts as a change of some kind.
    #[must_use]
    pub const fn is_change(self) -> bool {
        !matches!(self, Self::Unchanged | Self::Gap)
    }
}

/// Colors the merge view paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Background of a row neither side changed.
    pub unchanged_line: Color32,
    /// Text of a row neither side changed.
    pub unchanged_text: Color32,
    /// Background of a row the left side changed.
    pub left_line: Color32,
    /// Text of a row the left side changed.
    pub left_text: Color32,
    /// Background of a row the right side changed.
    pub right_line: Color32,
    /// Text of a row the right side changed.
    pub right_text: Color32,
    /// Background of a row both sides changed the same way.
    pub same_change_line: Color32,
    /// Text of a row both sides changed the same way.
    pub same_change_text: Color32,
    /// Background of a row the two sides changed differently.
    pub conflict_line: Color32,
    /// Text of a row the two sides changed differently.
    pub conflict_text: Color32,
    /// Background of an output section the user typed into.
    pub edited_line: Color32,
    /// Text of an output section the user typed into.
    pub edited_text: Color32,
    /// Background of a filler row.
    pub gap_line: Color32,
    /// Line color of the hatch drawn over a filler row.
    pub gap_pattern: Color32,
}

impl Palette {
    /// Background and text color for one row class.
    #[must_use]
    pub const fn row(&self, class: MergeClass) -> (Color32, Color32) {
        match class {
            MergeClass::Unchanged => (self.unchanged_line, self.unchanged_text),
            MergeClass::LeftChange => (self.left_line, self.left_text),
            MergeClass::RightChange => (self.right_line, self.right_text),
            MergeClass::SameChange => (self.same_change_line, self.same_change_text),
            MergeClass::Conflict => (self.conflict_line, self.conflict_text),
            MergeClass::Edited => (self.edited_line, self.edited_text),
            MergeClass::Gap => (self.gap_line, self.unchanged_text),
        }
    }
}

/// The light table. Provisional.
pub const LIGHT: Palette = Palette {
    unchanged_line: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    unchanged_text: Color32::from_rgb(0x10, 0x10, 0x10),
    left_line: Color32::from_rgb(0xDE, 0xF2, 0xF0),
    left_text: Color32::from_rgb(0x00, 0x60, 0x60),
    right_line: Color32::from_rgb(0xF6, 0xE2, 0xF6),
    right_text: Color32::from_rgb(0x80, 0x00, 0x80),
    same_change_line: Color32::from_rgb(0xE6, 0xEE, 0xE6),
    same_change_text: Color32::from_rgb(0x20, 0x60, 0x20),
    conflict_line: Color32::from_rgb(0xFF, 0xE4, 0xE4),
    conflict_text: Color32::from_rgb(0xC0, 0x00, 0x00),
    edited_line: Color32::from_rgb(0xFF, 0xF4, 0xCC),
    edited_text: Color32::from_rgb(0x70, 0x50, 0x00),
    gap_line: Color32::from_rgb(0xF6, 0xF6, 0xF6),
    gap_pattern: Color32::from_rgb(0xBC, 0xBC, 0xBC),
};

/// The dark table.
pub const DARK: Palette = Palette {
    unchanged_line: Color32::from_rgb(0x1B, 0x1E, 0x24),
    unchanged_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    left_line: Color32::from_rgb(0x19, 0x33, 0x45),
    left_text: Color32::from_rgb(0x67, 0xAD, 0xDD),
    right_line: Color32::from_rgb(0x3F, 0x2C, 0x15),
    right_text: Color32::from_rgb(0xF2, 0xB7, 0x72),
    same_change_line: Color32::from_rgb(0x1C, 0x33, 0x2E),
    same_change_text: Color32::from_rgb(0x9B, 0xC9, 0xBF),
    conflict_line: Color32::from_rgb(0x56, 0x23, 0x23),
    conflict_text: Color32::from_rgb(0xFF, 0x88, 0x7A),
    edited_line: Color32::from_rgb(0x36, 0x2E, 0x46),
    edited_text: Color32::from_rgb(0xDF, 0xCC, 0xFF),
    gap_line: Color32::from_rgb(0x1E, 0x22, 0x28),
    gap_pattern: Color32::from_rgb(0x49, 0x4D, 0x54),
};

/// The merge table for `variant`.
#[must_use]
pub const fn palette(variant: Variant) -> Palette {
    match variant {
        Variant::Light => LIGHT,
        Variant::Dark => DARK,
    }
}

impl crate::thumbnail::Severity for MergeClass {
    fn rank(self) -> u8 {
        match self {
            MergeClass::Unchanged | MergeClass::Gap => 0,
            MergeClass::SameChange => 1,
            MergeClass::LeftChange | MergeClass::RightChange => 2,
            MergeClass::Edited => 3,
            MergeClass::Conflict => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{palette, MergeClass, DARK, LIGHT};
    use crate::theme::Variant;

    #[test]
    fn every_class_names_a_pair_of_colors_in_both_tables() {
        for class in MergeClass::ALL {
            for variant in [Variant::Light, Variant::Dark] {
                let (background, text) = palette(variant).row(class);
                assert_ne!(background, text, "{} in {variant:?}", class.label());
            }
        }
    }

    #[test]
    fn the_two_sides_never_share_a_color() {
        for table in [LIGHT, DARK] {
            assert_ne!(table.left_line, table.right_line);
            assert_ne!(table.left_text, table.right_text);
            assert_ne!(table.conflict_line, table.left_line);
            assert_ne!(table.edited_line, table.conflict_line);
        }
    }

    #[test]
    fn a_conflict_outranks_every_other_class_in_the_strip() {
        use crate::thumbnail::Severity;
        for class in MergeClass::ALL {
            assert!(MergeClass::Conflict.rank() >= class.rank());
        }
    }
}
