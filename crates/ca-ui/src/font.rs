//! The point size one view draws its rows at.
//!
//! The size comes from the options document. A zoom command moves it away from
//! that value for the life of the view; the reset command puts it back. The
//! stored value is re-read every frame, so an edit in the options dialog reaches
//! an open view at once unless a zoom command has moved that view since.

use ca_session::options::provisional::{MAXIMUM_POINT_SIZE, MINIMUM_POINT_SIZE};

/// Point size of small chrome labels.
pub const CHROME_SMALL_POINT_SIZE: f32 = 9.0;
/// Point size of body and button chrome.
pub const CHROME_BODY_POINT_SIZE: f32 = 12.5;
/// Point size of chrome headings.
pub const CHROME_HEADING_POINT_SIZE: f32 = 18.0;
/// Point size of the default monospace text role.
pub const CHROME_MONOSPACE_POINT_SIZE: f32 = 12.0;
/// Shared line-height ratio for file content panes.
pub const CONTENT_ROW_HEIGHT_RATIO: f32 = 1.45;

/// Apply the shared proportional and monospace size scale to an egui context.
pub fn apply_chrome_scale(context: &egui::Context) {
    context.style_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::proportional(CHROME_SMALL_POINT_SIZE),
        );
        let body = egui::FontId::proportional(CHROME_BODY_POINT_SIZE);
        style
            .text_styles
            .insert(egui::TextStyle::Body, body.clone());
        style.text_styles.insert(egui::TextStyle::Button, body);
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::proportional(CHROME_HEADING_POINT_SIZE),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::monospace(CHROME_MONOSPACE_POINT_SIZE),
        );
    });
}

/// Smallest point size a view draws at.
///
/// The bound is the one the options page accepts, so a size reached by zooming
/// and a size typed into the dialog have the same range.
#[allow(clippy::cast_possible_truncation)]
pub const SMALLEST: f32 = MINIMUM_POINT_SIZE as f32;

/// Largest point size a view draws at.
#[allow(clippy::cast_possible_truncation)]
pub const LARGEST: f32 = MAXIMUM_POINT_SIZE as f32;

/// One step of a zoom command, in points.
pub const STEP: f32 = 1.0;

/// The point size of one view, following the options document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontSize {
    configured: f32,
    current: f32,
    pixels_per_point: f32,
    zoomed: bool,
}

impl FontSize {
    /// A size that follows `configured`.
    #[must_use]
    pub fn new(configured: f32) -> Self {
        let configured = clamp(configured);
        Self {
            configured,
            current: configured,
            pixels_per_point: 1.0,
            zoomed: false,
        }
    }

    /// Set the display scale used to snap row heights to physical pixels.
    pub fn set_pixels_per_point(&mut self, pixels_per_point: f32) {
        if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
            self.pixels_per_point = pixels_per_point;
        }
    }

    /// Take the size the options document now states.
    ///
    /// A view that has been zoomed keeps what the zoom reached until the stored
    /// value itself changes, so an edit in the dialog is never lost behind a
    /// zoom and a zoom is never undone by an unrelated frame.
    pub fn follow(&mut self, configured: f32) {
        let configured = clamp(configured);
        if (configured - self.configured).abs() < f32::EPSILON {
            return;
        }
        self.configured = configured;
        self.current = configured;
        self.zoomed = false;
    }

    /// The size the view draws at.
    #[must_use]
    pub const fn points(&self) -> f32 {
        self.current
    }

    /// The size the options document states.
    #[must_use]
    pub const fn configured(&self) -> f32 {
        self.configured
    }

    /// True while a zoom command has moved the size off the stored value.
    #[must_use]
    pub const fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Move the size by `steps` steps.
    pub fn zoom(&mut self, steps: f32) {
        self.set(self.current + steps * STEP);
    }

    /// Put the size back to what the options document states.
    pub fn reset(&mut self) {
        self.current = self.configured;
        self.zoomed = false;
    }

    /// Draw at an exact size, whatever the stored value is.
    pub fn set(&mut self, points: f32) {
        self.current = clamp(points);
        self.zoomed = (self.current - self.configured).abs() >= f32::EPSILON;
    }

    /// Answer one of the three zoom commands, reporting whether it applied.
    pub fn run(&mut self, command: crate::command::Command) -> bool {
        use crate::command::Command;
        match command {
            Command::IncreaseFontSize => self.zoom(1.0),
            Command::DecreaseFontSize => self.zoom(-1.0),
            Command::ResetFontSize => self.reset(),
            _ => return false,
        }
        true
    }

    /// The height of one row at this size, snapped to the physical pixel grid.
    #[must_use]
    pub fn row_height(&self, ratio: f32, extra_spacing: u32) -> f32 {
        let extra = f32::from(u16::try_from(extra_spacing).unwrap_or(u16::MAX));
        (self.current.mul_add(ratio, extra) * self.pixels_per_point)
            .round()
            .max(1.0)
            / self.pixels_per_point
    }
}

fn clamp(points: f32) -> f32 {
    if points.is_nan() {
        return SMALLEST;
    }
    points.clamp(SMALLEST, LARGEST)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{FontSize, LARGEST, SMALLEST};
    use crate::command::Command;

    #[test]
    fn a_new_size_draws_at_the_configured_value() {
        let size = FontSize::new(12.0);
        assert!((size.points() - 12.0).abs() < f32::EPSILON);
        assert!(!size.is_zoomed());
    }

    #[test]
    fn a_changed_option_reaches_the_view_and_a_zoom_survives_an_unchanged_one() {
        let mut size = FontSize::new(12.0);
        size.follow(12.0);
        assert!(size.run(Command::IncreaseFontSize));
        assert!((size.points() - 13.0).abs() < f32::EPSILON);
        size.follow(12.0);
        assert!(
            (size.points() - 13.0).abs() < f32::EPSILON,
            "an unchanged option undid the zoom"
        );
        size.follow(20.0);
        assert!((size.points() - 20.0).abs() < f32::EPSILON);
        assert!(!size.is_zoomed());
    }

    #[test]
    fn the_reset_command_returns_to_the_configured_value() {
        let mut size = FontSize::new(12.0);
        size.run(Command::DecreaseFontSize);
        size.run(Command::DecreaseFontSize);
        assert!((size.points() - 10.0).abs() < f32::EPSILON);
        assert!(size.is_zoomed());
        assert!(size.run(Command::ResetFontSize));
        assert!((size.points() - 12.0).abs() < f32::EPSILON);
        assert!(!size.is_zoomed());
    }

    #[test]
    fn a_size_stays_inside_the_range_the_options_page_accepts() {
        let mut size = FontSize::new(1.0);
        assert!((size.points() - SMALLEST).abs() < f32::EPSILON);
        size.set(1_000.0);
        assert!((size.points() - LARGEST).abs() < f32::EPSILON);
        size.set(f32::NAN);
        assert!((size.points() - SMALLEST).abs() < f32::EPSILON);
    }

    #[test]
    fn a_command_that_is_not_a_zoom_is_refused() {
        let mut size = FontSize::new(12.0);
        assert!(!size.run(Command::Find));
        assert!((size.points() - 12.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_row_height_follows_the_size_and_the_extra_spacing() {
        let mut size = FontSize::new(10.0);
        assert!((size.row_height(1.45, 0) - 15.0).abs() < f32::EPSILON);
        assert!((size.row_height(1.45, 3) - 18.0).abs() < f32::EPSILON);
        size.zoom(4.0);
        assert!((size.row_height(1.45, 0) - 20.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_row_height_snaps_to_the_physical_pixel_grid() {
        let mut size = FontSize::new(10.5);
        for (scale, expected_pixels) in [(1.0, 15.0), (1.25, 19.0), (1.5, 23.0), (2.0, 30.0)] {
            size.set_pixels_per_point(scale);
            let height = size.row_height(1.45, 0);
            let pixels = height * scale;
            assert!(
                (pixels - expected_pixels).abs() < 0.0001,
                "physical row height {pixels} at {scale} pixels per point did not match {expected_pixels}"
            );
        }
        assert!((size.row_height(1.45, 0) - 15.0).abs() < f32::EPSILON);
    }

    #[test]
    fn an_invalid_display_scale_does_not_break_row_height() {
        let mut size = FontSize::new(10.5);
        size.set_pixels_per_point(f32::NAN);
        size.set_pixels_per_point(0.0);
        assert!((size.row_height(1.45, 0) - 15.0).abs() < f32::EPSILON);
    }

    #[test]
    fn chrome_roles_use_the_shared_type_scale() {
        let context = egui::Context::default();
        super::apply_chrome_scale(&context);
        let style = context.style();
        assert_eq!(
            style.text_styles.get(&egui::TextStyle::Small),
            Some(&egui::FontId::proportional(super::CHROME_SMALL_POINT_SIZE))
        );
        assert_eq!(
            style.text_styles.get(&egui::TextStyle::Body),
            Some(&egui::FontId::proportional(super::CHROME_BODY_POINT_SIZE))
        );
        assert_eq!(
            style.text_styles.get(&egui::TextStyle::Button),
            Some(&egui::FontId::proportional(super::CHROME_BODY_POINT_SIZE))
        );
        assert_eq!(
            style.text_styles.get(&egui::TextStyle::Heading),
            Some(&egui::FontId::proportional(
                super::CHROME_HEADING_POINT_SIZE
            ))
        );
        assert_eq!(
            style.text_styles.get(&egui::TextStyle::Monospace),
            Some(&egui::FontId::monospace(super::CHROME_MONOSPACE_POINT_SIZE))
        );
    }
}
