//! Colors of the picture comparison view, for both theme variants.
//!
//! Only colors the view paints its own chrome with live here. The tints of the
//! difference image itself are comparison settings, not theme values, so they
//! are read from the picture settings and never from this table.

use super::Variant;
use egui::Color32;

/// Colors the picture comparison view paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// View background, behind every panel.
    pub background: Color32,
    /// Background of one image pane, where no image covers it.
    pub pane_background: Color32,
    /// Outline of a pane that does not hold the keyboard focus.
    pub pane_border: Color32,
    /// Outline of the pane that holds the keyboard focus.
    pub pane_border_focused: Color32,
    /// Background of the control column and the status bar.
    pub panel_background: Color32,
    /// Text of a field name.
    pub label_text: Color32,
    /// Text of a field value.
    pub value_text: Color32,
    /// Text of a message that stands for work that failed.
    pub error_text: Color32,
    /// Text of a message that qualifies a result without denying it.
    pub notice_text: Color32,
    /// Text of a message that stands for work in progress.
    pub progress_text: Color32,
    /// Crosshair drawn in the pane the pointer is over.
    pub crosshair: Color32,
    /// Crosshair drawn at the matching position in the other panes.
    pub crosshair_echo: Color32,
    /// Line separating two panels.
    pub separator: Color32,
}

/// Light variant.
pub const LIGHT: Palette = Palette {
    background: Color32::from_rgb(0xF0, 0xF0, 0xF0),
    pane_background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    pane_border: Color32::from_rgb(0xB4, 0xB4, 0xB4),
    pane_border_focused: Color32::from_rgb(0x00, 0x60, 0xC0),
    panel_background: Color32::from_rgb(0xE8, 0xE8, 0xE8),
    label_text: Color32::from_rgb(0x50, 0x50, 0x50),
    value_text: Color32::from_rgb(0x10, 0x10, 0x10),
    error_text: Color32::from_rgb(0xC0, 0x00, 0x00),
    notice_text: Color32::from_rgb(0x80, 0x50, 0x00),
    progress_text: Color32::from_rgb(0x40, 0x40, 0x40),
    crosshair: Color32::from_rgb(0x00, 0x60, 0xC0),
    crosshair_echo: Color32::from_rgb(0x80, 0x9C, 0xB8),
    separator: Color32::from_rgb(0xC0, 0xC0, 0xC0),
};

/// Dark variant.
pub const DARK: Palette = Palette {
    background: Color32::from_rgb(0x25, 0x29, 0x2F),
    pane_background: Color32::from_rgb(0x1B, 0x1E, 0x24),
    pane_border: Color32::from_rgb(0x39, 0x3D, 0x44),
    pane_border_focused: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    panel_background: Color32::from_rgb(0x2E, 0x32, 0x38),
    label_text: Color32::from_rgb(0xB6, 0xBB, 0xC2),
    value_text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    error_text: Color32::from_rgb(0xFB, 0x9F, 0x88),
    notice_text: Color32::from_rgb(0xFC, 0xCD, 0x73),
    progress_text: Color32::from_rgb(0x99, 0x9D, 0xA5),
    crosshair: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    crosshair_echo: Color32::from_rgb(0x56, 0x78, 0x96),
    separator: Color32::from_rgb(0x39, 0x3D, 0x44),
};

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
    use super::{palette, Variant, DARK, LIGHT};

    #[test]
    fn both_variants_separate_text_from_its_background() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            for text in [
                table.label_text,
                table.value_text,
                table.error_text,
                table.notice_text,
                table.progress_text,
            ] {
                assert_ne!(text, table.panel_background);
                assert_ne!(text, table.background);
            }
        }
    }

    #[test]
    fn the_two_tables_differ() {
        assert_ne!(LIGHT.pane_background, DARK.pane_background);
        assert_ne!(LIGHT.panel_background, DARK.panel_background);
    }

    #[test]
    fn a_focused_pane_is_outlined_differently() {
        for variant in [Variant::Light, Variant::Dark] {
            let table = palette(variant);
            assert_ne!(table.pane_border, table.pane_border_focused);
            assert_ne!(table.crosshair, table.crosshair_echo);
        }
    }
}
