//! Every color the comparison views paint with.
//!
//! Each class is a named constant in one of two tables and nothing else in any
//! crate may hold a literal color. Replacing a value is a one line edit here.

pub mod chrome;
#[cfg(test)]
mod contrast;
pub mod folder_merge;
pub mod hex;
pub mod icon;
pub mod merge;
pub mod picture;
pub mod records;
pub mod slots;
pub mod table;

use egui::Color32;

/// Which of the two tables is in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    /// Light background.
    #[default]
    Light,
    /// Dark background.
    Dark,
}

impl Variant {
    /// The variant matching an egui visuals setting.
    #[must_use]
    pub const fn from_dark_mode(dark: bool) -> Self {
        if dark {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

/// The classes a text comparison row can be painted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextClass {
    /// Lines identical on both sides.
    Same,
    /// Lines that differ and hold at least one important difference.
    Important,
    /// Lines that differ only in text classified unimportant.
    Unimportant,
    /// A line present on this side with no counterpart.
    Orphan,
    /// Filler standing in for a line the other side has and this one does not.
    Gap,
}

/// The classes a folder comparison row can be painted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderClass {
    /// Comparison has not run, or the item is older than its counterpart.
    Unknown,
    /// The two sides match.
    Same,
    /// The two sides differ.
    Different,
    /// The item exists on one side only.
    Orphan,
    /// The item could not be compared.
    Error,
}

/// One resolved color table.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Text of a line holding an important difference.
    pub important_text: Color32,
    /// Background tint of a line holding an important difference.
    pub important_line: Color32,
    /// Text of a line holding only unimportant differences, focused pane.
    pub unimportant_text: Color32,
    /// Background tint of a line holding only unimportant differences, focused
    /// pane.
    pub unimportant_line: Color32,
    /// Text of a line holding only unimportant differences, other pane.
    pub unimportant_text_other: Color32,
    /// Background tint of a line holding only unimportant differences, other
    /// pane.
    pub unimportant_line_other: Color32,
    /// Text of a line with no counterpart on the other side.
    pub orphan_text: Color32,
    /// Background tint of a line with no counterpart.
    pub orphan_line: Color32,
    /// Line color of the hatch drawn over filler rows.
    pub gap_pattern: Color32,
    /// Text of a line identical on both sides, focused pane.
    pub same_text: Color32,
    /// Background of a line identical on both sides, focused pane.
    pub same_line: Color32,
    /// Text of a line identical on both sides, other pane.
    pub same_text_other: Color32,
    /// Background of a line identical on both sides, other pane.
    pub same_line_other: Color32,
    /// Background of the row holding the caret when that row differs.
    pub caret_line: Color32,
    /// Tint applied to alternating rows.
    pub stripe: Color32,
    /// Line number gutter text.
    pub gutter_text: Color32,
    /// Line number gutter background.
    pub gutter_background: Color32,
    /// Copy arrow drawn in the gutter.
    pub gutter_arrow: Color32,
    /// Window chrome: status bar, detail area and the space below the rows.
    pub chrome: Color32,
    /// Toolbar background.
    pub toolbar: Color32,
    /// The stronger of the two lines that separate panes.
    pub separator_strong: Color32,
    /// The weaker of the two lines that separate panes, and the edges of the
    /// caret row.
    pub separator: Color32,
    /// Background of the thumbnail strip.
    pub thumbnail_background: Color32,
    /// Outline of the thumbnail's viewport marker.
    pub thumbnail_marker: Color32,
    /// Marker drawn at the caret's position in the thumbnail.
    pub thumbnail_caret: Color32,
    /// Folder row text for an item whose state is not known.
    pub folder_unknown: Color32,
    /// Folder row text for an item that matches.
    pub folder_same: Color32,
    /// Folder row text for an item that differs.
    pub folder_different: Color32,
    /// Folder row text for an item present on one side only.
    pub folder_orphan: Color32,
    /// Folder row text for an item that could not be compared.
    pub folder_error: Color32,
    /// Row background behind the center status column.
    pub center_column: Color32,
    /// Folder row background on the rows a stripe does not cover.
    pub folder_background: Color32,
    /// Folder column header background.
    pub folder_header_background: Color32,
    /// Folder column header text.
    pub folder_header_text: Color32,
    /// Folder row background behind a selected row.
    pub folder_selection: Color32,
    /// Line along the bottom edge of a selected folder row.
    pub folder_selection_edge: Color32,
    /// Row background behind the selected row.
    pub selection: Color32,
    /// Label of a settings field whose value this session states itself.
    pub settings_overridden: Color32,
    /// Label of a settings field taking its value from the layer below.
    pub settings_inherited: Color32,
    /// Text and icon of a notice that informs.
    pub notice_info: Color32,
    /// Text and icon of a notice that warns.
    pub notice_warning: Color32,
    /// Text and icon of a notice that reports a failure.
    pub notice_error: Color32,
    /// Syntax colors, one per highlight slot.
    pub syntax: SyntaxColors,
}

/// One color for every highlight slot a grammar can name.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxColors {
    /// Text no element claims.
    pub plain: Color32,
    /// Whitespace a view may reveal.
    pub whitespace: Color32,
    /// A comment in any of its forms.
    pub comment: Color32,
    /// A string or character literal.
    pub literal: Color32,
    /// A numeric literal.
    pub number: Color32,
    /// A reserved word.
    pub keyword: Color32,
    /// A user-chosen name.
    pub identifier: Color32,
    /// A preprocessor or build-time directive.
    pub directive: Color32,
    /// Punctuation that carries meaning.
    pub operator: Color32,
    /// The name of a markup tag.
    pub tag: Color32,
    /// The name of an attribute or key.
    pub attribute: Color32,
    /// A named division of a file.
    pub section: Color32,
    /// A repeating multi-line block.
    pub block: Color32,
    /// An element with no specific role.
    pub other: Color32,
}

/// The light table.
pub const LIGHT: Palette = Palette {
    important_text: Color32::from_rgb(0xC0, 0x00, 0x00),
    important_line: Color32::from_rgb(0xFF, 0xE4, 0xE4),
    unimportant_text: Color32::from_rgb(0x00, 0x00, 0xC0),
    unimportant_line: Color32::from_rgb(0xE4, 0xE8, 0xFF),
    unimportant_text_other: Color32::from_rgb(0x2C, 0x2C, 0xA8),
    unimportant_line_other: Color32::from_rgb(0xDC, 0xE0, 0xF6),
    orphan_text: Color32::from_rgb(0xC0, 0x00, 0x00),
    orphan_line: Color32::from_rgb(0xFF, 0xE4, 0xE4),
    gap_pattern: Color32::from_rgb(0xBC, 0xBC, 0xBC),
    same_text: Color32::from_rgb(0x10, 0x10, 0x10),
    same_line: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    same_text_other: Color32::from_rgb(0x3C, 0x3C, 0x3C),
    same_line_other: Color32::from_rgb(0xF6, 0xF6, 0xF6),
    caret_line: Color32::from_rgb(0xFF, 0xD4, 0xD4),
    stripe: Color32::from_rgb(0xF4, 0xF4, 0xF4),
    gutter_text: Color32::from_rgb(0x6C, 0x6C, 0x6C),
    gutter_background: Color32::from_rgb(0xEE, 0xEE, 0xEE),
    gutter_arrow: Color32::from_rgb(0xAF, 0x81, 0x00),
    chrome: Color32::from_rgb(0xE4, 0xE4, 0xE4),
    toolbar: Color32::from_rgb(0xF0, 0xF0, 0xF0),
    separator_strong: Color32::from_rgb(0x9A, 0x9A, 0x9A),
    separator: Color32::from_rgb(0xBE, 0xBE, 0xBE),
    thumbnail_background: Color32::from_rgb(0xE0, 0xE0, 0xE0),
    thumbnail_marker: Color32::from_rgb(0x20, 0x20, 0x20),
    thumbnail_caret: Color32::from_rgb(0x00, 0x60, 0xC0),
    folder_unknown: Color32::from_rgb(0x61, 0x61, 0x61),
    folder_same: Color32::from_rgb(0x10, 0x10, 0x10),
    folder_different: Color32::from_rgb(0xC0, 0x00, 0x00),
    folder_orphan: Color32::from_rgb(0x80, 0x00, 0x80),
    folder_error: Color32::from_rgb(0xBA, 0x29, 0x00),
    center_column: Color32::from_rgb(0xEE, 0xEE, 0xEE),
    folder_background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    folder_header_background: Color32::from_rgb(0xE8, 0xE8, 0xE8),
    folder_header_text: Color32::from_rgb(0x10, 0x10, 0x10),
    folder_selection: Color32::from_rgb(0xCC, 0xDE, 0xF6),
    folder_selection_edge: Color32::from_rgb(0x8A, 0xB0, 0xDE),
    selection: Color32::from_rgb(0xCC, 0xDE, 0xF6),
    settings_overridden: Color32::from_rgb(0x10, 0x30, 0x80),
    settings_inherited: Color32::from_rgb(0x60, 0x60, 0x60),
    notice_info: Color32::from_rgb(0x00, 0x50, 0xA8),
    notice_warning: Color32::from_rgb(0x80, 0x50, 0x00),
    notice_error: Color32::from_rgb(0xB0, 0x00, 0x00),
    syntax: SyntaxColors {
        plain: Color32::from_rgb(0x10, 0x10, 0x10),
        whitespace: Color32::from_rgb(0x8E, 0x8E, 0x8E),
        comment: Color32::from_rgb(0x00, 0x80, 0x00),
        literal: Color32::from_rgb(0xA0, 0x30, 0x20),
        number: Color32::from_rgb(0x10, 0x60, 0xA0),
        keyword: Color32::from_rgb(0x00, 0x00, 0xC0),
        identifier: Color32::from_rgb(0x10, 0x10, 0x10),
        directive: Color32::from_rgb(0x80, 0x50, 0x00),
        operator: Color32::from_rgb(0x50, 0x50, 0x50),
        tag: Color32::from_rgb(0x00, 0x00, 0xA0),
        attribute: Color32::from_rgb(0x70, 0x00, 0x70),
        section: Color32::from_rgb(0x00, 0x60, 0x80),
        block: Color32::from_rgb(0x50, 0x40, 0x20),
        other: Color32::from_rgb(0x30, 0x30, 0x30),
    },
};

/// The dark table.
pub const DARK: Palette = Palette {
    important_text: Color32::from_rgb(0xFB, 0x9F, 0x88),
    important_line: Color32::from_rgb(0x4A, 0x23, 0x23),
    unimportant_text: Color32::from_rgb(0x94, 0xAF, 0xC3),
    unimportant_line: Color32::from_rgb(0x20, 0x30, 0x3D),
    unimportant_text_other: Color32::from_rgb(0x91, 0xA8, 0xB9),
    unimportant_line_other: Color32::from_rgb(0x20, 0x2D, 0x38),
    orphan_text: Color32::from_rgb(0xCA, 0xAC, 0xFF),
    orphan_line: Color32::from_rgb(0x35, 0x29, 0x49),
    gap_pattern: Color32::from_rgb(0x49, 0x4D, 0x54),
    same_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    same_line: Color32::from_rgb(0x1B, 0x1E, 0x24),
    same_text_other: Color32::from_rgb(0xCA, 0xCE, 0xD4),
    same_line_other: Color32::from_rgb(0x1E, 0x22, 0x28),
    caret_line: Color32::from_rgb(0x2E, 0x37, 0x45),
    stripe: Color32::from_rgb(0x20, 0x24, 0x29),
    gutter_text: Color32::from_rgb(0x99, 0x9D, 0xA5),
    gutter_background: Color32::from_rgb(0x21, 0x25, 0x2A),
    gutter_arrow: Color32::from_rgb(0xF2, 0xCD, 0x6F),
    chrome: Color32::from_rgb(0x25, 0x29, 0x2F),
    toolbar: Color32::from_rgb(0x2E, 0x32, 0x38),
    separator_strong: Color32::from_rgb(0x51, 0x55, 0x5C),
    separator: Color32::from_rgb(0x39, 0x3D, 0x44),
    thumbnail_background: Color32::from_rgb(0x25, 0x29, 0x2F),
    thumbnail_marker: Color32::from_rgb(0xCD, 0xD1, 0xD8),
    thumbnail_caret: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    folder_unknown: Color32::from_rgb(0xA2, 0xA5, 0xAA),
    folder_same: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    folder_different: Color32::from_rgb(0xFB, 0x9F, 0x88),
    folder_orphan: Color32::from_rgb(0xCA, 0xAC, 0xFF),
    folder_error: Color32::from_rgb(0xFC, 0xCD, 0x73),
    center_column: Color32::from_rgb(0x21, 0x25, 0x2A),
    folder_background: Color32::from_rgb(0x1B, 0x1E, 0x24),
    folder_header_background: Color32::from_rgb(0x2E, 0x32, 0x38),
    folder_header_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    folder_selection: Color32::from_rgb(0x18, 0x33, 0x56),
    folder_selection_edge: Color32::from_rgb(0x43, 0x78, 0xAD),
    selection: Color32::from_rgb(0x18, 0x33, 0x56),
    settings_overridden: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    settings_inherited: Color32::from_rgb(0xB6, 0xBB, 0xC2),
    notice_info: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    notice_warning: Color32::from_rgb(0xFC, 0xCD, 0x73),
    notice_error: Color32::from_rgb(0xFB, 0x9F, 0x88),
    syntax: SyntaxColors {
        plain: Color32::from_rgb(0xDD, 0xE1, 0xE8),
        whitespace: Color32::from_rgb(0x68, 0x6C, 0x72),
        comment: Color32::from_rgb(0x8C, 0xB8, 0x94),
        literal: Color32::from_rgb(0xEA, 0xB4, 0x89),
        number: Color32::from_rgb(0x83, 0xD4, 0xD8),
        keyword: Color32::from_rgb(0xAA, 0xB1, 0xF7),
        identifier: Color32::from_rgb(0xDD, 0xE1, 0xE8),
        directive: Color32::from_rgb(0xE0, 0xC0, 0x7B),
        operator: Color32::from_rgb(0xBA, 0xBE, 0xC4),
        tag: Color32::from_rgb(0x8E, 0xBF, 0xF3),
        attribute: Color32::from_rgb(0xD8, 0xAB, 0xE2),
        section: Color32::from_rgb(0x80, 0xD1, 0xD1),
        block: Color32::from_rgb(0xD1, 0xBA, 0x9B),
        other: Color32::from_rgb(0xC3, 0xC7, 0xCE),
    },
};

/// The table for `variant`.
#[must_use]
pub const fn palette(variant: Variant) -> Palette {
    match variant {
        Variant::Light => LIGHT,
        Variant::Dark => DARK,
    }
}

/// The shade laid over the pane without focus, darkening it by `fraction`,
/// from zero (no change) to one (black).
#[must_use]
pub fn dim_overlay(fraction: f32) -> Color32 {
    // The clamp keeps the product inside the range of a byte.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let alpha = (fraction.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_black_alpha(alpha)
}

impl Palette {
    /// Background and text color for a text comparison row in the focused pane.
    #[must_use]
    pub const fn text_row(&self, class: TextClass) -> (Color32, Color32) {
        self.text_row_in(class, true)
    }

    /// Background and text color for a text comparison row.
    ///
    /// The focused pane and the other pane carry different values for matching
    /// text and for unimportant differences. Important differences and orphans
    /// carry one value for both panes.
    #[must_use]
    pub const fn text_row_in(&self, class: TextClass, focused: bool) -> (Color32, Color32) {
        match class {
            TextClass::Same | TextClass::Gap => {
                if focused {
                    (self.same_line, self.same_text)
                } else {
                    (self.same_line_other, self.same_text_other)
                }
            }
            TextClass::Important => (self.important_line, self.important_text),
            TextClass::Unimportant => {
                if focused {
                    (self.unimportant_line, self.unimportant_text)
                } else {
                    (self.unimportant_line_other, self.unimportant_text_other)
                }
            }
            TextClass::Orphan => (self.orphan_line, self.orphan_text),
        }
    }

    /// Text color for one syntax highlight slot.
    #[must_use]
    pub const fn syntax_slot(&self, slot: ca_grammar::StyleSlot) -> Color32 {
        use ca_grammar::StyleSlot;
        match slot {
            StyleSlot::Plain => self.syntax.plain,
            StyleSlot::Whitespace => self.syntax.whitespace,
            StyleSlot::Comment => self.syntax.comment,
            StyleSlot::Literal => self.syntax.literal,
            StyleSlot::Number => self.syntax.number,
            StyleSlot::Keyword => self.syntax.keyword,
            StyleSlot::Identifier => self.syntax.identifier,
            StyleSlot::Directive => self.syntax.directive,
            StyleSlot::Operator => self.syntax.operator,
            StyleSlot::Tag => self.syntax.tag,
            StyleSlot::Attribute => self.syntax.attribute,
            StyleSlot::Section => self.syntax.section,
            StyleSlot::Block => self.syntax.block,
            StyleSlot::Other => self.syntax.other,
        }
    }

    /// Text color for a folder comparison row.
    #[must_use]
    pub const fn folder_row(&self, class: FolderClass) -> Color32 {
        match class {
            FolderClass::Unknown => self.folder_unknown,
            FolderClass::Same => self.folder_same,
            FolderClass::Different => self.folder_different,
            FolderClass::Orphan => self.folder_orphan,
            FolderClass::Error => self.folder_error,
        }
    }
}

/// The painting class of a folder comparison status.
#[must_use]
pub const fn folder_class(status: ca_fs::NodeStatus) -> FolderClass {
    use ca_fs::NodeStatus;
    match status {
        NodeStatus::NotCompared => FolderClass::Unknown,
        NodeStatus::Same => FolderClass::Same,
        NodeStatus::Different | NodeStatus::LeftNewer | NodeStatus::RightNewer => {
            FolderClass::Different
        }
        NodeStatus::LeftOrphan | NodeStatus::RightOrphan => FolderClass::Orphan,
        NodeStatus::KindMismatch | NodeStatus::Error => FolderClass::Error,
    }
}

impl crate::thumbnail::Severity for TextClass {
    fn rank(self) -> u8 {
        match self {
            TextClass::Same | TextClass::Gap => 0,
            TextClass::Unimportant => 1,
            TextClass::Orphan => 2,
            TextClass::Important => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{folder_class, palette, FolderClass, TextClass, Variant, DARK, LIGHT};

    #[test]
    fn both_variants_resolve_every_text_class() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for class in [
                TextClass::Same,
                TextClass::Important,
                TextClass::Unimportant,
                TextClass::Orphan,
                TextClass::Gap,
            ] {
                let (background, text) = table.text_row(class);
                assert_ne!(background, text, "{variant:?} {class:?} is unreadable");
            }
        }
    }

    #[test]
    fn the_two_panes_carry_different_backgrounds() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for class in [TextClass::Same, TextClass::Unimportant] {
                let focused = table.text_row_in(class, true);
                let other = table.text_row_in(class, false);
                assert_ne!(focused.0, other.0, "{variant:?} {class:?} background");
                assert_ne!(focused.1, other.1, "{variant:?} {class:?} text");
            }
            for class in [TextClass::Important, TextClass::Orphan] {
                assert_eq!(
                    table.text_row_in(class, true),
                    table.text_row_in(class, false)
                );
            }
        }
    }

    #[test]
    fn the_two_tables_differ() {
        assert_ne!(LIGHT.same_line, DARK.same_line);
        assert_ne!(LIGHT.important_line, DARK.important_line);
    }

    #[test]
    fn a_folder_row_is_readable_against_its_own_backgrounds() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for class in [
                FolderClass::Same,
                FolderClass::Different,
                FolderClass::Orphan,
                FolderClass::Error,
                FolderClass::Unknown,
            ] {
                let text = table.folder_row(class);
                for background in [
                    table.folder_background,
                    table.stripe,
                    table.folder_selection,
                ] {
                    assert_ne!(text, background, "{variant:?} {class:?} is unreadable");
                }
            }
            assert_ne!(table.folder_header_text, table.folder_header_background);
            assert_ne!(table.folder_selection, table.folder_selection_edge);
        }
    }

    #[test]
    fn every_status_maps_to_a_class() {
        use ca_fs::NodeStatus;
        assert_eq!(folder_class(NodeStatus::Same), FolderClass::Same);
        assert_eq!(folder_class(NodeStatus::LeftOrphan), FolderClass::Orphan);
        assert_eq!(folder_class(NodeStatus::LeftNewer), FolderClass::Different);
        assert_eq!(folder_class(NodeStatus::NotCompared), FolderClass::Unknown);
        assert_eq!(folder_class(NodeStatus::Error), FolderClass::Error);
    }
}
