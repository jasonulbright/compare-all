//! Shared layout values for chrome, controls and comparison panes.

/// Values applied to egui's shared spacing style.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EguiTokens {
    /// Gap between neighboring controls, including button rows.
    pub item_spacing: egui::Vec2,
    /// Padding inside native and embedded windows.
    pub window_margin: egui::Margin,
    /// Padding inside menus.
    pub menu_margin: egui::Margin,
    /// Padding around button contents.
    pub button_padding: egui::Vec2,
    /// Indent for egui tree and collapsing controls.
    pub indent: f32,
    /// Minimum clickable control size.
    pub interact_size: egui::Vec2,
    /// Default slider width.
    pub slider_width: f32,
    /// Default slider rail height.
    pub slider_rail_height: f32,
    /// Minimum combo box width.
    pub combo_width: f32,
    /// Default text edit width.
    pub text_edit_width: f32,
    /// Width of a checkbox or radio icon.
    pub icon_width: f32,
    /// Width of the inner checkbox or radio mark.
    pub icon_inner_width: f32,
    /// Gap between an icon and its label.
    pub icon_spacing: f32,
    /// Default area size used during its first layout pass.
    pub default_area_size: egui::Vec2,
    /// Tooltip width.
    pub tooltip_width: f32,
    /// Menu width.
    pub menu_width: f32,
    /// Gap between menus and submenus.
    pub menu_spacing: f32,
    /// Height before a combo box adds a scrollbar.
    pub combo_height: f32,
    /// Whether an indented region ends with a horizontal line.
    pub indent_ends_with_horizontal_line: bool,
}

/// Toolbar icon sizes and control allocation widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolbarTokens {
    /// Icon size above a toolbar label.
    pub icon_above: f32,
    /// Icon size beside an inline toolbar label.
    pub icon_inline: f32,
    /// Width reserved for the toolbar overflow control.
    pub overflow_width: f32,
    /// Left offset for an icon in a menu button.
    pub menu_icon_left_offset: f32,
    /// Size of a toolbar menu icon.
    pub menu_icon_size: f32,
    /// Hit target size of a small icon toggle.
    pub toggle_hit_size: f32,
    /// Width of Hex Compare toolbar combo boxes.
    pub hex_combo_width: f32,
    /// Width of Picture Compare toolbar combo boxes.
    pub picture_combo_width: f32,
    /// Width of Picture Compare toolbar sliders.
    pub picture_slider_width: f32,
    /// Width of Picture Compare numeric fields.
    pub picture_field_width: f32,
    /// Widths shared by toolbar entries in several views.
    pub common_items: CommonToolbarItemTokens,
    /// Folder Compare toolbar entry widths.
    pub folder_items: FolderToolbarItemTokens,
    /// Text Compare toolbar entry widths.
    pub text_items: TextToolbarItemTokens,
    /// Hex Compare toolbar entry widths.
    pub hex_items: HexToolbarItemTokens,
    /// Picture Compare toolbar entry widths.
    pub picture_items: PictureToolbarItemTokens,
    /// Records Compare toolbar entry widths.
    pub records_items: RecordsToolbarItemTokens,
    /// Table Compare toolbar entry widths.
    pub table_items: TableToolbarItemTokens,
    /// Text Merge toolbar entry widths.
    pub merge_items: MergeToolbarItemTokens,
    /// Folder Merge toolbar entry widths.
    pub folder_merge_items: FolderMergeToolbarItemTokens,
}

/// Toolbar widths reused by more than one view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommonToolbarItemTokens {
    /// Home control.
    pub home: f32,
    /// Saved sessions control.
    pub sessions: f32,
    /// All rows filter.
    pub all: f32,
    /// Differences filter.
    pub differences: f32,
    /// Same rows filter.
    pub same: f32,
    /// Minor differences control.
    pub minor: f32,
    /// Rules control.
    pub rules: f32,
    /// Peek control.
    pub peek: f32,
}

/// Folder Compare toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FolderToolbarItemTokens {
    /// Display filter combo box.
    pub filter: f32,
    /// Folder structure combo box.
    pub structure: f32,
    /// Expand control.
    pub expand: f32,
    /// Collapse control.
    pub collapse: f32,
    /// Selection control.
    pub select: f32,
    /// File operation control.
    pub files: f32,
    /// Content method combo box.
    pub method: f32,
    /// Name filter control.
    pub name_filter: f32,
    /// Synchronization method combo box.
    pub sync_method: f32,
}

/// Text Compare toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextToolbarItemTokens {
    /// Context row control.
    pub context: f32,
    /// Rules control.
    pub rules: f32,
    /// Format control.
    pub format: f32,
}

/// Hex Compare toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HexToolbarItemTokens {
    /// Filter control including its label.
    pub filter: f32,
    /// Width control.
    pub width: f32,
    /// Hex address control.
    pub hex_addresses: f32,
    /// Address display control.
    pub addresses: f32,
    /// Thumbnail control.
    pub thumbnail: f32,
    /// File information control.
    pub file_info: f32,
    /// Text Compare launch control.
    pub text_compare: f32,
    /// Parent folder control.
    pub parent_folders: f32,
    /// Font control.
    pub font: f32,
}

/// Picture Compare toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureToolbarItemTokens {
    /// Zoom in control.
    pub zoom_in: f32,
    /// Zoom out control.
    pub zoom_out: f32,
    /// Actual size control.
    pub actual_size: f32,
    /// Fit control.
    pub fit: f32,
    /// Pane controls.
    pub panes: f32,
    /// Overflow control.
    pub more: f32,
}

/// Records Compare toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordsToolbarItemTokens {
    /// Display filter combo box.
    pub filter: f32,
    /// Minor differences control.
    pub minor: f32,
    /// Difference strip control.
    pub strip: f32,
    /// Details control.
    pub details: f32,
    /// Hex display control.
    pub hex: f32,
}

/// Table Compare toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TableToolbarItemTokens {
    /// Display filter combo box.
    pub filter: f32,
    /// Minor differences control.
    pub minor: f32,
    /// Hide same columns control.
    pub hide_same: f32,
    /// Unhide column control.
    pub unhide_column: f32,
    /// Row number control.
    pub row_numbers: f32,
    /// Difference strip control.
    pub strip: f32,
    /// Details control.
    pub details: f32,
}

/// Text Merge toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MergeToolbarItemTokens {
    /// Display filter combo box.
    pub filter: f32,
}

/// Folder Merge toolbar widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FolderMergeToolbarItemTokens {
    /// Display filter combo box.
    pub filter: f32,
}

/// Widths used by forms, launch rows and their controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormTokens {
    /// Label column width shared by forms in native dialogs.
    pub dialog_label_width: f32,
    /// Options form control width.
    pub options_control_width: f32,
    /// Settings form control width.
    pub settings_control_width: f32,
    /// Minimum form label width when the window is narrow.
    pub minimum_label_width: f32,
    /// Minimum settings control width.
    pub minimum_control_width: f32,
    /// Workspace name field width.
    pub workspace_name_width: f32,
    /// Padding reserved at the end of an elided workspace name.
    pub workspace_name_elision_padding: f32,
    /// Launcher saved-session row name width.
    pub launcher_name_width: f32,
    /// Width of the launcher name editor.
    pub launcher_name_edit_width: f32,
    /// Width reserved at the end of an elided launcher name.
    pub launcher_name_elision_padding: f32,
    /// Horizontal room kept around dialogs.
    pub dialog_screen_margin: f32,
}

/// Shared measurements used inside comparison panes and trees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneTokens {
    /// Indent added at each tree depth.
    pub tree_indent: f32,
    /// Gap between table and record panes.
    pub pane_separator: f32,
    /// Width of the draggable folder pane splitter.
    pub folder_splitter_width: f32,
    /// Space between Text Compare panes.
    pub text_center_gap: f32,
    /// Width of each Text Compare center separator line.
    pub text_center_separator_line_width: f32,
    /// Space between Hex Compare panes.
    pub hex_center_gap: f32,
    /// Gap used by the three-way text merge pane layout.
    pub merge_pane_gap: f32,
    /// Padding between cell text and its cell edge.
    pub cell_padding: f32,
    /// Margin inside cell frames.
    pub cell_frame_margin: i8,
    /// Margin inside inline editor frames.
    pub editor_frame_margin: i8,
    /// Margin used by folder operation dialogs.
    pub folder_dialog_margin: f32,
}

/// The one table of shared layout values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    /// Values applied to egui's shared style.
    pub egui: EguiTokens,
    /// Toolbar sizes.
    pub toolbar: ToolbarTokens,
    /// Form and launcher widths.
    pub forms: FormTokens,
    /// Tree and pane measurements.
    pub panes: PaneTokens,
    /// Explicit margins around the shell's central panel.
    pub shell_margin: egui::Margin,
}

/// The shared spacing table. The egui values match the measured baseline.
pub const TOKENS: Tokens = Tokens {
    egui: EguiTokens {
        item_spacing: egui::Vec2::new(8.0, 3.0),
        window_margin: egui::Margin::same(6),
        menu_margin: egui::Margin::same(6),
        button_padding: egui::Vec2::new(4.0, 1.0),
        indent: 18.0,
        interact_size: egui::Vec2::new(40.0, 18.0),
        slider_width: 100.0,
        slider_rail_height: 8.0,
        combo_width: 100.0,
        text_edit_width: 280.0,
        icon_width: 14.0,
        icon_inner_width: 8.0,
        icon_spacing: 4.0,
        default_area_size: egui::Vec2::new(600.0, 400.0),
        tooltip_width: 500.0,
        menu_width: 400.0,
        menu_spacing: 2.0,
        combo_height: 200.0,
        indent_ends_with_horizontal_line: false,
    },
    toolbar: ToolbarTokens {
        icon_above: 24.0,
        icon_inline: 16.0,
        overflow_width: 34.0,
        menu_icon_left_offset: 8.0,
        menu_icon_size: 16.0,
        toggle_hit_size: 20.0,
        hex_combo_width: 116.0,
        picture_combo_width: 148.0,
        picture_slider_width: 160.0,
        picture_field_width: 84.0,
        common_items: CommonToolbarItemTokens {
            home: 70.0,
            sessions: 90.0,
            all: 46.0,
            differences: 56.0,
            same: 56.0,
            minor: 60.0,
            rules: 60.0,
            peek: 60.0,
        },
        folder_items: FolderToolbarItemTokens {
            filter: 230.0,
            structure: 250.0,
            expand: 80.0,
            collapse: 90.0,
            select: 80.0,
            files: 60.0,
            method: 120.0,
            name_filter: 190.0,
            sync_method: 170.0,
        },
        text_items: TextToolbarItemTokens {
            context: 130.0,
            rules: 70.0,
            format: 74.0,
        },
        hex_items: HexToolbarItemTokens {
            filter: 176.0,
            width: 140.0,
            hex_addresses: 120.0,
            addresses: 100.0,
            thumbnail: 100.0,
            file_info: 90.0,
            text_compare: 120.0,
            parent_folders: 130.0,
            font: 140.0,
        },
        picture_items: PictureToolbarItemTokens {
            zoom_in: 90.0,
            zoom_out: 100.0,
            actual_size: 60.0,
            fit: 60.0,
            panes: 320.0,
            more: 80.0,
        },
        records_items: RecordsToolbarItemTokens {
            filter: 150.0,
            minor: 70.0,
            strip: 110.0,
            details: 90.0,
            hex: 110.0,
        },
        table_items: TableToolbarItemTokens {
            filter: 160.0,
            minor: 70.0,
            hide_same: 160.0,
            unhide_column: 150.0,
            row_numbers: 120.0,
            strip: 110.0,
            details: 100.0,
        },
        merge_items: MergeToolbarItemTokens { filter: 150.0 },
        folder_merge_items: FolderMergeToolbarItemTokens { filter: 170.0 },
    },
    forms: FormTokens {
        dialog_label_width: 220.0,
        options_control_width: 170.0,
        settings_control_width: 180.0,
        minimum_label_width: 80.0,
        minimum_control_width: 60.0,
        workspace_name_width: 220.0,
        workspace_name_elision_padding: 8.0,
        launcher_name_width: 260.0,
        launcher_name_edit_width: 180.0,
        launcher_name_elision_padding: 24.0,
        dialog_screen_margin: 24.0,
    },
    panes: PaneTokens {
        tree_indent: 14.0,
        pane_separator: 6.0,
        folder_splitter_width: 6.0,
        text_center_gap: 2.0,
        text_center_separator_line_width: 1.0,
        hex_center_gap: 14.0,
        merge_pane_gap: 2.0,
        cell_padding: 4.0,
        cell_frame_margin: 4,
        editor_frame_margin: 1,
        folder_dialog_margin: 12.0,
    },
    shell_margin: egui::Margin::same(8),
};

/// Apply the shared egui spacing values to both built-in themes.
pub fn install(context: &egui::Context) {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let current = context.style_of(theme);
        let mut updated = current.as_ref().clone();
        apply_egui_tokens(&mut updated.spacing);
        if updated.spacing != current.spacing {
            context.set_style_of(theme, updated);
        }
    }
}

/// Build the shell's central-panel frame with an explicit symmetric margin.
pub fn shell_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::central_panel(style).inner_margin(TOKENS.shell_margin)
}

fn apply_egui_tokens(spacing: &mut egui::Spacing) {
    let tokens = TOKENS.egui;
    spacing.item_spacing = tokens.item_spacing;
    spacing.window_margin = tokens.window_margin;
    spacing.menu_margin = tokens.menu_margin;
    spacing.button_padding = tokens.button_padding;
    spacing.indent = tokens.indent;
    spacing.interact_size = tokens.interact_size;
    spacing.slider_width = tokens.slider_width;
    spacing.slider_rail_height = tokens.slider_rail_height;
    spacing.combo_width = tokens.combo_width;
    spacing.text_edit_width = tokens.text_edit_width;
    spacing.icon_width = tokens.icon_width;
    spacing.icon_width_inner = tokens.icon_inner_width;
    spacing.icon_spacing = tokens.icon_spacing;
    spacing.default_area_size = tokens.default_area_size;
    spacing.tooltip_width = tokens.tooltip_width;
    spacing.menu_width = tokens.menu_width;
    spacing.menu_spacing = tokens.menu_spacing;
    spacing.combo_height = tokens.combo_height;
    spacing.indent_ends_with_horizontal_line = tokens.indent_ends_with_horizontal_line;
}

#[cfg(test)]
mod tests {
    use super::{shell_frame, TOKENS};

    fn assert_same_float(actual: f32, expected: f32) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }

    #[test]
    fn installs_one_spacing_table_for_both_themes() {
        let context = egui::Context::default();
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            context.style_mut_of(theme, |style| {
                style.spacing.item_spacing = egui::Vec2::splat(1.0);
            });
        }

        crate::theme::chrome::install(&context);

        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let spacing = &context.style_of(theme).spacing;
            assert_eq!(spacing.item_spacing, TOKENS.egui.item_spacing);
            assert_eq!(spacing.window_margin, TOKENS.egui.window_margin);
            assert_eq!(spacing.menu_margin, TOKENS.egui.menu_margin);
            assert_eq!(spacing.button_padding, TOKENS.egui.button_padding);
            assert_same_float(spacing.indent, TOKENS.egui.indent);
            assert_eq!(spacing.interact_size, TOKENS.egui.interact_size);
            assert_same_float(spacing.slider_width, TOKENS.egui.slider_width);
            assert_same_float(spacing.slider_rail_height, TOKENS.egui.slider_rail_height);
            assert_same_float(spacing.combo_width, TOKENS.egui.combo_width);
            assert_same_float(spacing.text_edit_width, TOKENS.egui.text_edit_width);
            assert_same_float(spacing.icon_width, TOKENS.egui.icon_width);
            assert_same_float(spacing.icon_width_inner, TOKENS.egui.icon_inner_width);
            assert_same_float(spacing.icon_spacing, TOKENS.egui.icon_spacing);
            assert_eq!(spacing.default_area_size, TOKENS.egui.default_area_size);
            assert_same_float(spacing.tooltip_width, TOKENS.egui.tooltip_width);
            assert_same_float(spacing.menu_width, TOKENS.egui.menu_width);
            assert_same_float(spacing.menu_spacing, TOKENS.egui.menu_spacing);
            assert_same_float(spacing.combo_height, TOKENS.egui.combo_height);
            assert_eq!(
                spacing.indent_ends_with_horizontal_line,
                TOKENS.egui.indent_ends_with_horizontal_line
            );
        }
    }

    #[test]
    fn shell_margin_is_symmetric_and_keeps_the_central_panel_frame() {
        let context = egui::Context::default();
        let frame = shell_frame(context.style().as_ref());
        let margin = frame.inner_margin;

        assert_eq!(margin.left, margin.right);
        assert_eq!(margin.top, margin.bottom);
        assert_eq!(margin, TOKENS.shell_margin);
    }
}
