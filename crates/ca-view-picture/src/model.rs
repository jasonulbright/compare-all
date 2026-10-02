//! Zoom and pan arithmetic, and the mapping between a pointer and a pixel.
//!
//! Nothing here paints or allocates, so every rule below is testable without a
//! frame. One [`Camera`] drives all three panes, which is what keeps them
//! synchronized: the panes share a magnification and a scroll position and
//! differ only in where their content sits in the shared coordinate space.
//!
//! The shared space is the left image's pixel grid. A pane states the position
//! of its own pixel (0, 0) in that space, so a result buffer whose origin is
//! negative still lines up with the image it was compared from.

/// Smallest magnification the view allows.
pub const MIN_ZOOM: f32 = 0.01;

/// Largest magnification the view allows.
pub const MAX_ZOOM: f32 = 32.0;

/// Factor one zoom step multiplies or divides the magnification by.
const ZOOM_STEP: f32 = std::f32::consts::SQRT_2;

/// Where the content is looked at from, shared by every pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Screen points per image pixel.
    pub zoom: f32,
    /// Shared-space coordinate drawn at each pane's top-left corner.
    pub origin: [f32; 2],
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            origin: [0.0, 0.0],
        }
    }
}

/// `zoom` brought inside the range the view allows.
#[must_use]
pub fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_nan() {
        return 1.0;
    }
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

impl Camera {
    /// A camera at actual size showing the top-left corner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The shared-space point drawn at `screen`, for a pane placed at
    /// `pane_min`.
    #[must_use]
    pub fn point_at(&self, pane_min: [f32; 2], screen: [f32; 2]) -> [f32; 2] {
        let zoom = clamp_zoom(self.zoom);
        [
            (screen[0] - pane_min[0]) / zoom + self.origin[0],
            (screen[1] - pane_min[1]) / zoom + self.origin[1],
        ]
    }

    /// The screen position a shared-space point is drawn at.
    #[must_use]
    pub fn screen_of(&self, pane_min: [f32; 2], point: [f32; 2]) -> [f32; 2] {
        let zoom = clamp_zoom(self.zoom);
        [
            (point[0] - self.origin[0]) * zoom + pane_min[0],
            (point[1] - self.origin[1]) * zoom + pane_min[1],
        ]
    }

    /// Move the content by a drag of `delta` screen points.
    ///
    /// The content follows the pointer, so the origin moves against the drag.
    pub fn pan_by_screen(&mut self, delta: [f32; 2]) {
        let zoom = clamp_zoom(self.zoom);
        self.origin[0] -= delta[0] / zoom;
        self.origin[1] -= delta[1] / zoom;
    }

    /// Change the magnification, keeping the point under `screen` in place.
    ///
    /// The fixed point is what makes a wheel zoom feel anchored: the pixel the
    /// pointer rests on is the one pixel whose screen position does not move.
    pub fn zoom_about(&mut self, pane_min: [f32; 2], screen: [f32; 2], zoom: f32) {
        let held = self.point_at(pane_min, screen);
        self.zoom = clamp_zoom(zoom);
        let after = self.point_at(pane_min, screen);
        self.origin[0] += held[0] - after[0];
        self.origin[1] += held[1] - after[1];
    }

    /// One step in, anchored at `screen`.
    pub fn zoom_in_about(&mut self, pane_min: [f32; 2], screen: [f32; 2]) {
        self.zoom_about(pane_min, screen, self.zoom * ZOOM_STEP);
    }

    /// One step out, anchored at `screen`.
    pub fn zoom_out_about(&mut self, pane_min: [f32; 2], screen: [f32; 2]) {
        self.zoom_about(pane_min, screen, self.zoom / ZOOM_STEP);
    }

    /// Show `size` image pixels centered in a pane of `pane_size` points, at
    /// the magnification that makes the content fit.
    pub fn fit(&mut self, pane_size: [f32; 2], size: [u32; 2]) {
        let width = content_span(size[0]);
        let height = content_span(size[1]);
        if pane_size[0] <= 0.0 || pane_size[1] <= 0.0 {
            return;
        }
        self.zoom = clamp_zoom((pane_size[0] / width).min(pane_size[1] / height));
        self.center_on(pane_size, [width / 2.0, height / 2.0]);
    }

    /// Show the shared-space rectangle `bounds` centered in a pane of
    /// `pane_size` points, at the magnification that makes it fit.
    ///
    /// The rectangle is `[min_x, min_y, max_x, max_y]`. A pane states its own
    /// size here, so a fit stays correct when the panes differ in size: the
    /// smallest pane is the one that decides the magnification.
    pub fn fit_bounds(&mut self, pane_size: [f32; 2], bounds: [f32; 4]) {
        if pane_size[0] <= 0.0 || pane_size[1] <= 0.0 {
            return;
        }
        let width = (bounds[2] - bounds[0]).max(1.0);
        let height = (bounds[3] - bounds[1]).max(1.0);
        self.zoom = clamp_zoom((pane_size[0] / width).min(pane_size[1] / height));
        self.center_on(
            pane_size,
            [
                f32::midpoint(bounds[0], bounds[2]),
                f32::midpoint(bounds[1], bounds[3]),
            ],
        );
    }

    /// Show the content at one screen point per image pixel, keeping the
    /// shared-space point at the pane's center where it is.
    pub fn actual_size(&mut self, pane_size: [f32; 2]) {
        let center = [
            self.origin[0] + pane_size[0] / (2.0 * clamp_zoom(self.zoom)),
            self.origin[1] + pane_size[1] / (2.0 * clamp_zoom(self.zoom)),
        ];
        self.zoom = 1.0;
        self.center_on(pane_size, center);
    }

    /// Place `point` at the center of a pane of `pane_size` points.
    pub fn center_on(&mut self, pane_size: [f32; 2], point: [f32; 2]) {
        let zoom = clamp_zoom(self.zoom);
        self.origin = [
            point[0] - pane_size[0] / (2.0 * zoom),
            point[1] - pane_size[1] / (2.0 * zoom),
        ];
    }
}

/// A dimension as a float, never zero, so it can divide.
#[allow(clippy::cast_precision_loss)]
fn content_span(value: u32) -> f32 {
    (value as f32).max(1.0)
}

/// Where one pane's content sits in the shared coordinate space, and how large
/// it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Shared-space coordinate of the content's pixel (0, 0).
    pub origin: [i32; 2],
    /// Content size in pixels.
    pub size: [u32; 2],
}

impl Placement {
    /// A placement from its parts.
    #[must_use]
    pub fn new(origin: [i32; 2], size: [u32; 2]) -> Self {
        Self { origin, size }
    }

    /// The content pixel a shared-space point falls in, or `None` when the
    /// point lies outside the content.
    ///
    /// The coordinate is floored rather than rounded, so every screen position
    /// inside a magnified pixel names that pixel.
    #[must_use]
    pub fn pixel_of(&self, point: [f32; 2]) -> Option<[u32; 2]> {
        let x = floor_to_i64(point[0]).checked_sub(i64::from(self.origin[0]))?;
        let y = floor_to_i64(point[1]).checked_sub(i64::from(self.origin[1]))?;
        let x = u32::try_from(x).ok()?;
        let y = u32::try_from(y).ok()?;
        if x >= self.size[0] || y >= self.size[1] {
            return None;
        }
        Some([x, y])
    }

    /// The shared-space rectangle the content covers, as
    /// `[min_x, min_y, max_x, max_y]` with the maxima exclusive.
    #[must_use]
    pub fn bounds(&self) -> [f32; 4] {
        let min_x = f64::from(self.origin[0]);
        let min_y = f64::from(self.origin[1]);
        #[allow(clippy::cast_possible_truncation)]
        [
            min_x as f32,
            min_y as f32,
            (min_x + f64::from(self.size[0])) as f32,
            (min_y + f64::from(self.size[1])) as f32,
        ]
    }
}

/// `value` floored to a whole number, saturating rather than wrapping.
#[allow(clippy::cast_possible_truncation)]
fn floor_to_i64(value: f32) -> i64 {
    let floored = f64::from(value).floor();
    if floored >= 9.0e18 {
        return i64::MAX;
    }
    if floored <= -9.0e18 {
        return i64::MIN;
    }
    floored as i64
}

/// The content pixels a pane of `pane_size` points exposes, as a half-open
/// rectangle `[x0, y0, x1, y1]` in content coordinates.
///
/// One pixel of overscan is added on each side so a partly visible pixel is
/// still covered. An empty rectangle means the content is off screen.
#[must_use]
pub fn visible_pixels(camera: &Camera, pane_size: [f32; 2], placement: Placement) -> [u32; 4] {
    let zoom = clamp_zoom(camera.zoom);
    let min = [camera.origin[0], camera.origin[1]];
    let max = [
        camera.origin[0] + pane_size[0].max(0.0) / zoom,
        camera.origin[1] + pane_size[1].max(0.0) / zoom,
    ];
    let span = |low: f32, high: f32, origin: i32, size: u32| -> (u32, u32) {
        let start = floor_to_i64(low) - i64::from(origin) - 1;
        let end = floor_to_i64(high) - i64::from(origin) + 2;
        let start = start.clamp(0, i64::from(size));
        let end = end.clamp(0, i64::from(size));
        (
            u32::try_from(start).unwrap_or(0),
            u32::try_from(end.max(start)).unwrap_or(0),
        )
    };
    let (x0, x1) = span(min[0], max[0], placement.origin[0], placement.size[0]);
    let (y0, y1) = span(min[1], max[1], placement.origin[1], placement.size[1]);
    [x0, y0, x1, y1]
}

/// Which panes the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneToggles {
    /// Show the left image.
    pub left: bool,
    /// Show the right image.
    pub right: bool,
    /// Show the difference result.
    pub difference: bool,
}

impl Default for PaneToggles {
    fn default() -> Self {
        Self {
            left: true,
            right: true,
            difference: true,
        }
    }
}

impl PaneToggles {
    /// Turn one pane on or off, keeping at least one pane on.
    ///
    /// A layout with no pane at all shows nothing and offers no way back, so
    /// the last pane standing cannot be turned off.
    pub fn set(&mut self, pane: Pane, shown: bool) {
        let mut next = *self;
        match pane {
            Pane::Left => next.left = shown,
            Pane::Right => next.right = shown,
            Pane::Difference => next.difference = shown,
        }
        if next.count() > 0 {
            *self = next;
        }
    }

    /// True when `pane` is shown.
    #[must_use]
    pub fn shows(&self, pane: Pane) -> bool {
        match pane {
            Pane::Left => self.left,
            Pane::Right => self.right,
            Pane::Difference => self.difference,
        }
    }

    /// How many panes are shown.
    #[must_use]
    pub fn count(&self) -> usize {
        usize::from(self.left) + usize::from(self.right) + usize::from(self.difference)
    }
}

/// One of the three panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    /// The left image.
    Left,
    /// The right image.
    Right,
    /// The difference result.
    Difference,
}

impl Pane {
    /// Every pane, in the order they are laid out.
    pub const ALL: [Pane; 3] = [Pane::Left, Pane::Right, Pane::Difference];

    /// The name shown above the pane.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Pane::Left => "Left",
            Pane::Right => "Right",
            Pane::Difference => "Difference",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        clamp_zoom, visible_pixels, Camera, Pane, PaneToggles, Placement, MAX_ZOOM, MIN_ZOOM,
    };

    #[test]
    fn a_point_round_trips_through_the_screen_at_every_zoom() {
        for zoom in [0.1_f32, 0.25, 1.0, 3.0, 17.5] {
            let camera = Camera {
                zoom,
                origin: [12.5, -7.25],
            };
            let point = [40.0_f32, 31.0];
            let screen = camera.screen_of([100.0, 50.0], point);
            let back = camera.point_at([100.0, 50.0], screen);
            assert!((back[0] - point[0]).abs() < 1e-3, "zoom {zoom}");
            assert!((back[1] - point[1]).abs() < 1e-3, "zoom {zoom}");
        }
    }

    #[test]
    fn a_wheel_zoom_holds_the_pixel_under_the_pointer() {
        for zoom in [0.2_f32, 1.0, 8.0] {
            let mut camera = Camera {
                zoom,
                origin: [3.0, 9.0],
            };
            let pane = [10.0_f32, 20.0];
            let pointer = [137.0_f32, 211.0];
            let held = camera.point_at(pane, pointer);
            camera.zoom_in_about(pane, pointer);
            let after = camera.point_at(pane, pointer);
            assert!((held[0] - after[0]).abs() < 1e-2, "zoom {zoom}");
            assert!((held[1] - after[1]).abs() < 1e-2, "zoom {zoom}");
        }
    }

    #[test]
    fn zooming_stays_inside_the_allowed_range() {
        let mut camera = Camera::new();
        for _ in 0..200 {
            camera.zoom_in_about([0.0, 0.0], [0.0, 0.0]);
        }
        assert!((camera.zoom - MAX_ZOOM).abs() < 1e-3);
        for _ in 0..400 {
            camera.zoom_out_about([0.0, 0.0], [0.0, 0.0]);
        }
        assert!((camera.zoom - MIN_ZOOM).abs() < 1e-4);
        assert!((clamp_zoom(f32::NAN) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_drag_moves_the_content_with_the_pointer() {
        let mut camera = Camera {
            zoom: 2.0,
            origin: [0.0, 0.0],
        };
        camera.pan_by_screen([10.0, -4.0]);
        assert!((camera.origin[0] + 5.0).abs() < 1e-4);
        assert!((camera.origin[1] - 2.0).abs() < 1e-4);
    }

    #[test]
    fn fit_shows_the_whole_content_and_centers_it() {
        let mut camera = Camera::new();
        camera.fit([400.0, 200.0], [800, 800]);
        assert!((camera.zoom - 0.25).abs() < 1e-4);
        let top_left = camera.screen_of([0.0, 0.0], [0.0, 0.0]);
        let bottom_right = camera.screen_of([0.0, 0.0], [800.0, 800.0]);
        assert!(top_left[1] >= -1e-3 && bottom_right[1] <= 200.0 + 1e-3);
        let middle = camera.point_at([0.0, 0.0], [200.0, 100.0]);
        assert!((middle[0] - 400.0).abs() < 1e-2);
        assert!((middle[1] - 400.0).abs() < 1e-2);
    }

    #[test]
    fn fitting_bounds_centers_them_in_the_pane() {
        let mut camera = Camera::new();
        let pane = [300.0_f32, 200.0];
        camera.fit_bounds(pane, [-20.0, -10.0, 140.0, 90.0]);
        let top_left = camera.screen_of([0.0, 0.0], [-20.0, -10.0]);
        let bottom_right = camera.screen_of([0.0, 0.0], [140.0, 90.0]);
        assert!(top_left[0] >= -1e-3 && bottom_right[0] <= pane[0] + 1e-3);
        assert!(top_left[1] >= -1e-3 && bottom_right[1] <= pane[1] + 1e-3);
        // The margins are equal on both axes, which is what centering means.
        assert!((top_left[0] - (pane[0] - bottom_right[0])).abs() < 1e-2);
        assert!((top_left[1] - (pane[1] - bottom_right[1])).abs() < 1e-2);
    }

    #[test]
    fn actual_size_keeps_the_center_pixel() {
        let mut camera = Camera {
            zoom: 0.25,
            origin: [0.0, 0.0],
        };
        let pane = [400.0_f32, 200.0];
        let before = camera.point_at([0.0, 0.0], [200.0, 100.0]);
        camera.actual_size(pane);
        assert!((camera.zoom - 1.0).abs() < 1e-6);
        let after = camera.point_at([0.0, 0.0], [200.0, 100.0]);
        assert!((before[0] - after[0]).abs() < 1e-2);
        assert!((before[1] - after[1]).abs() < 1e-2);
    }

    #[test]
    fn a_pointer_maps_to_the_pixel_it_rests_on_at_every_zoom() {
        let placement = Placement::new([-4, -6], [20, 20]);
        for zoom in [0.5_f32, 1.0, 4.0, 16.0] {
            let camera = Camera {
                zoom,
                origin: [-4.0, -6.0],
            };
            let pane = [30.0_f32, 40.0];
            // The middle of the third pixel across and the second one down.
            let screen = [pane[0] + 2.5 * zoom, pane[1] + 1.5 * zoom];
            let point = camera.point_at(pane, screen);
            assert_eq!(placement.pixel_of(point), Some([2, 1]), "zoom {zoom}");
        }
    }

    #[test]
    fn a_pointer_outside_the_content_maps_to_no_pixel() {
        let placement = Placement::new([0, 0], [4, 4]);
        assert_eq!(placement.pixel_of([-0.5, 1.0]), None);
        assert_eq!(placement.pixel_of([4.0, 1.0]), None);
        assert_eq!(placement.pixel_of([1.0, 4.0]), None);
        assert_eq!(placement.pixel_of([0.0, 0.0]), Some([0, 0]));
        assert_eq!(placement.pixel_of([3.99, 3.99]), Some([3, 3]));
    }

    #[test]
    fn an_offset_placement_shifts_the_pixel_it_names() {
        let placement = Placement::new([10, -10], [4, 4]);
        assert_eq!(placement.pixel_of([10.5, -9.5]), Some([0, 0]));
        assert_eq!(placement.pixel_of([13.0, -7.0]), Some([3, 3]));
        assert_eq!(placement.pixel_of([9.5, -9.5]), None);
    }

    #[test]
    fn the_visible_rectangle_covers_the_pane_and_no_more() {
        let camera = Camera {
            zoom: 2.0,
            origin: [10.0, 10.0],
        };
        let placement = Placement::new([0, 0], [100, 100]);
        let [x0, y0, x1, y1] = visible_pixels(&camera, [40.0, 20.0], placement);
        assert!(x0 <= 10 && x1 >= 30);
        assert!(y0 <= 10 && y1 >= 20);
        assert!(x1 <= 100 && y1 <= 100);
    }

    #[test]
    fn content_entirely_off_screen_exposes_nothing() {
        let camera = Camera {
            zoom: 1.0,
            origin: [500.0, 500.0],
        };
        let placement = Placement::new([0, 0], [10, 10]);
        let [x0, y0, x1, y1] = visible_pixels(&camera, [40.0, 20.0], placement);
        assert_eq!((x1 - x0) * (y1 - y0), 0);
    }

    #[test]
    fn the_last_pane_cannot_be_turned_off() {
        let mut toggles = PaneToggles::default();
        toggles.set(Pane::Left, false);
        toggles.set(Pane::Right, false);
        assert_eq!(toggles.count(), 1);
        toggles.set(Pane::Difference, false);
        assert_eq!(toggles.count(), 1);
        assert!(toggles.shows(Pane::Difference));
        toggles.set(Pane::Left, true);
        assert_eq!(toggles.count(), 2);
    }
}
