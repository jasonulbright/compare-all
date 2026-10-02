//! Colors of the window chrome every view shares: panels, menus, buttons, text
//! fields and dialogs.
//!
//! The dark variant replaces the stock egui dark visuals. The light variant
//! keeps the stock egui light visuals.

use super::Variant;
use egui::{Color32, Stroke};

/// Colors the shared chrome paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Text fields and scroll tracks, one step below the panes.
    pub extreme: Color32,
    /// Panels, the menu bar and the space around the views.
    pub panel: Color32,
    /// Dialogs and popups, one step above the panels.
    pub window: Color32,
    /// Alternating rows of a striped grid.
    pub faint: Color32,
    /// Button background at rest.
    pub widget: Color32,
    /// Button background under the pointer.
    pub hovered: Color32,
    /// Button background while pressed.
    pub pressed: Color32,
    /// Separators and dialog edges.
    pub border: Color32,
    /// Edge of a dialog and of an open menu.
    pub border_strong: Color32,
    /// Text of labels.
    pub label: Color32,
    /// Text of buttons and text fields.
    pub text: Color32,
    /// Text of a button under the pointer or pressed.
    pub text_bright: Color32,
    /// Links, focus rings and the edge of a hovered text field.
    pub accent: Color32,
    /// Edge of a button under the pointer.
    pub accent_dim: Color32,
    /// Background of a toggled-on button and of selected text.
    pub accent_fill: Color32,
    /// Warning text.
    pub warning: Color32,
    /// Error text.
    pub error: Color32,
}

/// Dark variant.
pub const DARK: Palette = Palette {
    extreme: Color32::from_rgb(0x14, 0x17, 0x1D),
    panel: Color32::from_rgb(0x25, 0x29, 0x2F),
    window: Color32::from_rgb(0x2A, 0x2E, 0x34),
    faint: Color32::from_rgb(0x2A, 0x2E, 0x34),
    widget: Color32::from_rgb(0x3B, 0x3F, 0x45),
    hovered: Color32::from_rgb(0x48, 0x4C, 0x52),
    pressed: Color32::from_rgb(0x53, 0x57, 0x5E),
    border: Color32::from_rgb(0x39, 0x3D, 0x44),
    border_strong: Color32::from_rgb(0x51, 0x55, 0x5C),
    label: Color32::from_rgb(0xCA, 0xCE, 0xD4),
    text: Color32::from_rgb(0xDD, 0xE1, 0xE8),
    text_bright: Color32::from_rgb(0xEF, 0xF2, 0xF7),
    accent: Color32::from_rgb(0x6F, 0xB8, 0xF5),
    accent_dim: Color32::from_rgb(0x56, 0x78, 0x96),
    accent_fill: Color32::from_rgb(0x22, 0x5B, 0x8D),
    warning: Color32::from_rgb(0xFC, 0xCD, 0x73),
    error: Color32::from_rgb(0xFB, 0x9F, 0x88),
};

/// The egui visuals built from `palette` over the stock dark visuals.
#[must_use]
pub fn dark_visuals(palette: &Palette) -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = palette.panel;
    visuals.window_fill = palette.window;
    visuals.window_stroke = Stroke::new(1.0, palette.border_strong);
    visuals.extreme_bg_color = palette.extreme;
    visuals.faint_bg_color = palette.faint;
    visuals.code_bg_color = palette.widget;
    visuals.hyperlink_color = palette.accent;
    visuals.warn_fg_color = palette.warning;
    visuals.error_fg_color = palette.error;
    visuals.selection.bg_fill = palette.accent_fill;
    visuals.selection.stroke = Stroke::new(1.0, palette.text_bright);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.weak_bg_fill = palette.panel;
    widgets.noninteractive.bg_fill = palette.panel;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.label);

    widgets.inactive.weak_bg_fill = palette.widget;
    widgets.inactive.bg_fill = palette.widget;
    widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);

    widgets.hovered.weak_bg_fill = palette.hovered;
    widgets.hovered.bg_fill = palette.hovered;
    widgets.hovered.bg_stroke = Stroke::new(1.0, palette.accent_dim);
    widgets.hovered.fg_stroke = Stroke::new(1.5, palette.text_bright);

    widgets.active.weak_bg_fill = palette.pressed;
    widgets.active.bg_fill = palette.pressed;
    widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    widgets.active.fg_stroke = Stroke::new(2.0, palette.text_bright);

    widgets.open.weak_bg_fill = palette.widget;
    widgets.open.bg_fill = palette.panel;
    widgets.open.bg_stroke = Stroke::new(1.0, palette.border_strong);
    widgets.open.fg_stroke = Stroke::new(1.0, palette.text);
    visuals
}

/// The egui visuals for `variant`.
#[must_use]
pub fn visuals(variant: Variant) -> egui::Visuals {
    match variant {
        Variant::Light => egui::Visuals::light(),
        Variant::Dark => dark_visuals(&DARK),
    }
}

/// Put the dark chrome in force for every frame that paints dark.
///
/// The light visuals and the choice between the two stay with egui.
pub fn install(ctx: &egui::Context) {
    let wanted = visuals(Variant::Dark);
    if ctx.style_of(egui::Theme::Dark).visuals != wanted {
        ctx.set_visuals_of(egui::Theme::Dark, wanted);
    }
}
