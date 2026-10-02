//! The resolved options a frame runs under.
//!
//! The window resolves the stored document once per frame and puts the result
//! where every view can read it. A view asks for it by context rather than
//! through its own field, so changing an option reaches an open view on the
//! next frame without the view holding a copy that could go stale.

use crate::command::ShortcutTable;
use crate::theme::slots::Tables;
use crate::theme::Variant;
use ca_session::options::ProgramOptions;
use ca_session::AdminPolicies;
use std::sync::Arc;

/// Key the resolved options are held under.
fn slot() -> egui::Id {
    egui::Id::new("application-options")
}

/// Everything a frame reads out of the options document.
#[derive(Debug, Clone)]
pub struct AppOptions {
    /// The stored document as it stands.
    pub stored: ProgramOptions,
    /// The color tables the stored colors resolve to.
    pub tables: Tables,
    /// The routing table the stored shortcuts resolve to.
    pub shortcuts: ShortcutTable,
    /// The administrator policies in force.
    pub policies: AdminPolicies,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self::resolve(
            ProgramOptions::default(),
            Variant::Light,
            AdminPolicies::default(),
        )
    }
}

impl AppOptions {
    /// Resolves a stored document into the tables and the routing a frame uses.
    #[must_use]
    pub fn resolve(stored: ProgramOptions, variant: Variant, policies: AdminPolicies) -> Self {
        let tables = Tables::resolve(variant, &stored.appearance.palettes);
        let shortcuts = ShortcutTable::from_options(&stored.commands);
        Self {
            stored,
            tables,
            shortcuts,
            policies,
        }
    }

    /// The variant these tables were resolved for.
    #[must_use]
    pub const fn variant(&self) -> Variant {
        self.tables.variant
    }

    /// The point size one kind of view draws at.
    #[must_use]
    pub fn editor_point_size(&self) -> f32 {
        self.stored.appearance.fonts.editor_point_size
    }

    /// The point size a byte pane draws at.
    #[must_use]
    pub fn hex_point_size(&self) -> f32 {
        self.stored.appearance.fonts.hex_point_size
    }

    /// The point size a listing draws at.
    #[must_use]
    pub fn listing_point_size(&self) -> f32 {
        self.stored.appearance.fonts.listing_point_size
    }

    /// The point size a folder listing draws at, or nothing while the system
    /// font is in use.
    #[must_use]
    pub fn folder_point_size(&self) -> Option<f32> {
        let fonts = &self.stored.appearance.fonts;
        (!fonts.folder_uses_system_font).then_some(fonts.folder_point_size)
    }

    /// The point size a folder listing draws at.
    ///
    /// The system font is not reachable without a platform dependency this
    /// build does not take, so the listing size stands in for it.
    #[must_use]
    pub fn folder_row_point_size(&self) -> f32 {
        self.folder_point_size()
            .unwrap_or_else(|| self.listing_point_size())
    }

    /// The point size a merge input pane draws at.
    ///
    /// The inputs follow the editor unless the tweak states a font of their
    /// own, which is why the two sizes resolve in one place.
    #[must_use]
    pub fn merge_input_point_size(&self) -> f32 {
        if self.stored.tweaks.different_font_for_merge_input_panes {
            self.stored.appearance.fonts.merge_input_point_size
        } else {
            self.editor_point_size()
        }
    }

    /// Padding added between rows of text, in points.
    #[must_use]
    pub const fn extra_line_spacing(&self) -> u32 {
        self.stored.tweaks.extra_line_spacing
    }

    /// The column a vertical ruler is drawn at, where one is drawn.
    #[must_use]
    pub const fn column_line_at(&self) -> Option<u32> {
        match self.stored.tweaks.column_line_at {
            0 => None,
            column => Some(column),
        }
    }

    /// The fraction the pane without focus is darkened by, from zero to one.
    #[must_use]
    pub fn dim_inactive_pane(&self) -> f32 {
        f32::from(u16::try_from(self.stored.tweaks.dim_inactive_pane_percent).unwrap_or(100))
            .clamp(0.0, 100.0)
            / 100.0
    }
}

/// Put the resolved options where this frame's views can read them.
pub fn install(ctx: &egui::Context, options: Arc<AppOptions>) {
    ctx.data_mut(|data| data.insert_temp(slot(), options));
}

/// The options in force, or the built-in ones when nothing installed any.
#[must_use]
pub fn current(ctx: &egui::Context) -> Arc<AppOptions> {
    ctx.data_mut(|data| data.get_temp::<Arc<AppOptions>>(slot()))
        .unwrap_or_else(|| Arc::new(AppOptions::default()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{current, install, AppOptions};
    use crate::theme::slots::to_stored;
    use crate::theme::Variant;
    use ca_session::options::{ColorGroup, ProgramOptions, Rgb};
    use ca_session::AdminPolicies;
    use std::sync::Arc;

    #[test]
    fn a_context_with_nothing_installed_reads_the_built_in_options() {
        let ctx = egui::Context::default();
        let options = current(&ctx);
        assert_eq!(options.stored, ProgramOptions::default());
    }

    #[test]
    fn installed_options_reach_the_next_read() {
        let ctx = egui::Context::default();
        let mut stored = ProgramOptions::default();
        stored.text_editing.tab_stop = 3;
        stored
            .appearance
            .palettes
            .group_mut(ColorGroup::Text)
            .table_mut(true)
            .set("same_line", Rgb::new(7, 7, 7));
        install(
            &ctx,
            Arc::new(AppOptions::resolve(
                stored,
                Variant::Dark,
                AdminPolicies::default(),
            )),
        );
        let options = current(&ctx);
        assert_eq!(options.stored.text_editing.tab_stop, 3);
        assert_eq!(to_stored(options.tables.main.same_line), Rgb::new(7, 7, 7));
        assert_eq!(options.variant(), Variant::Dark);
    }

    #[test]
    fn the_folder_point_size_is_absent_while_the_system_font_is_in_use() {
        let options = AppOptions::default();
        assert_eq!(options.folder_point_size(), None);
        let mut stored = ProgramOptions::default();
        stored.appearance.fonts.folder_uses_system_font = false;
        stored.appearance.fonts.folder_point_size = 17.0;
        let options = AppOptions::resolve(stored, Variant::Light, AdminPolicies::default());
        assert_eq!(options.folder_point_size(), Some(17.0));
        assert!((options.folder_row_point_size() - 17.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_merge_inputs_follow_the_editor_until_the_tweak_states_otherwise() {
        let mut stored = ProgramOptions::default();
        stored.appearance.fonts.editor_point_size = 11.0;
        stored.appearance.fonts.merge_input_point_size = 20.0;
        let options = AppOptions::resolve(stored.clone(), Variant::Light, AdminPolicies::default());
        assert!((options.merge_input_point_size() - 11.0).abs() < f32::EPSILON);

        stored.tweaks.different_font_for_merge_input_panes = true;
        let options = AppOptions::resolve(stored, Variant::Light, AdminPolicies::default());
        assert!((options.merge_input_point_size() - 20.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_column_line_of_zero_draws_no_ruler_and_the_dim_percentage_is_a_fraction() {
        let mut stored = ProgramOptions::default();
        stored.tweaks.column_line_at = 0;
        stored.tweaks.dim_inactive_pane_percent = 40;
        let options = AppOptions::resolve(stored.clone(), Variant::Light, AdminPolicies::default());
        assert_eq!(options.column_line_at(), None);
        assert!((options.dim_inactive_pane() - 0.4).abs() < 0.001);

        stored.tweaks.column_line_at = 80;
        let options = AppOptions::resolve(stored, Variant::Light, AdminPolicies::default());
        assert_eq!(options.column_line_at(), Some(80));
    }
}
