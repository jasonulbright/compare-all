//! The point size one view draws its rows at.
//!
//! The size comes from the options document. A zoom command moves it away from
//! that value for the life of the view; the reset command puts it back. The
//! stored value is re-read every frame, so an edit in the options dialog reaches
//! an open view at once unless a zoom command has moved that view since.

use ca_session::options::provisional::{MAXIMUM_POINT_SIZE, MINIMUM_POINT_SIZE};

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
            zoomed: false,
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

    /// The height of one row at this size, with the padding the options state.
    #[must_use]
    pub fn row_height(&self, ratio: f32, extra_spacing: u32) -> f32 {
        let extra = f32::from(u16::try_from(extra_spacing).unwrap_or(u16::MAX));
        (self.current * ratio).round().max(1.0) + extra
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
}
