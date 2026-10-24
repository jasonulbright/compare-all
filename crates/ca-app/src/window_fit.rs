//! Keeps the window inside the work area of its monitor.
//!
//! The work area is the monitor without the taskbar. A new window gets its
//! position from the system, which does not look at the taskbar, so a window
//! can open with its bottom edge below the work area. Every rectangle here is
//! in screen coordinates and egui points, which is the scale of
//! [`egui::ViewportInfo`].

use egui::{Pos2, Rect, Vec2, ViewportInfo};

/// The most plan steps one trigger may send. The system can refuse a size,
/// such as one below the minimum window size, and then the same plan repeats.
const MAXIMUM_ATTEMPTS: u8 = 4;

/// A difference below this many points counts as no difference.
const TOLERANCE: f32 = 1.0;

/// Returns where the work area of the monitor under `outer` is, in points.
///
/// `None` where the system gives no such value.
pub type WorkAreaSource = fn(outer: Rect, pixels_per_point: f32) -> Option<Rect>;

/// The work area of the monitor that holds most of `outer`.
///
/// Only Windows answers. Other window managers place a new window inside the
/// usable area themselves.
#[cfg(windows)]
#[must_use]
pub fn work_area_of(outer: Rect, pixels_per_point: f32) -> Option<Rect> {
    use winsafe::prelude::*;
    use winsafe::{co, HMONITOR, RECT};

    if !(pixels_per_point.is_finite() && pixels_per_point > 0.0 && is_usable(outer)) {
        return None;
    }
    // Screen coordinates stay far inside the range of an i32 and of an f32
    // mantissa, so the conversions below lose nothing.
    #[allow(clippy::cast_possible_truncation)]
    let to_pixels = |points: f32| (points * pixels_per_point).round() as i32;
    #[allow(clippy::cast_precision_loss)]
    let to_points = |pixels: i32| pixels as f32 / pixels_per_point;
    let probe = RECT {
        left: to_pixels(outer.min.x),
        top: to_pixels(outer.min.y),
        right: to_pixels(outer.max.x),
        bottom: to_pixels(outer.max.y),
    };
    let info = HMONITOR::MonitorFromRect(probe, co::MONITOR::DEFAULTTONEAREST)
        .GetMonitorInfo()
        .ok()?;
    let work = info.rcWork;
    let area = Rect::from_min_max(
        Pos2::new(to_points(work.left), to_points(work.top)),
        Pos2::new(to_points(work.right), to_points(work.bottom)),
    );
    is_usable(area).then_some(area)
}

/// The work area of the monitor that holds most of `outer`.
///
/// Only Windows answers. Other window managers place a new window inside the
/// usable area themselves.
#[cfg(not(windows))]
#[must_use]
pub fn work_area_of(_outer: Rect, _pixels_per_point: f32) -> Option<Rect> {
    None
}

fn is_usable(rect: Rect) -> bool {
    rect.min.is_finite() && rect.max.is_finite() && rect.width() > 0.0 && rect.height() > 0.0
}

/// `outer` moved and cut down so that it lies inside `area`.
///
/// The size shrinks to the size of the area where it is larger. The position
/// moves the least that puts the right and bottom edges inside, then the left
/// and top edges, so a window larger than the area keeps its top left corner.
#[must_use]
pub fn fit_into(outer: Rect, area: Rect) -> Rect {
    if !is_usable(outer) || !is_usable(area) {
        return outer;
    }
    let size = outer.size().min(area.size());
    let x = outer.min.x.min(area.max.x - size.x).max(area.min.x);
    let y = outer.min.y.min(area.max.y - size.y).max(area.min.y);
    Rect::from_min_size(Pos2::new(x, y), size)
}

/// The part of the client area of a window that shows inside `area`.
///
/// `inner` is the client area in screen coordinates. The result is in client
/// coordinates, which is the space of the egui screen rectangle. `None` where
/// no part shows.
#[must_use]
pub fn visible_client(inner: Rect, area: Rect) -> Option<Rect> {
    if !is_usable(inner) || !is_usable(area) {
        return None;
    }
    let shown = inner.intersect(area);
    if shown.width() <= 0.0 || shown.height() <= 0.0 {
        return None;
    }
    Some(shown.translate(-inner.min.to_vec2()))
}

/// How far each edge of the egui screen rectangle lies outside the part of
/// the window that shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    /// Hidden width on the left.
    pub left: f32,
    /// Hidden width on the right.
    pub right: f32,
    /// Hidden height at the bottom.
    pub bottom: f32,
}

impl Insets {
    /// The edges of `screen` that `visible` leaves out.
    #[must_use]
    pub fn between(screen: Rect, visible: Rect) -> Self {
        Self {
            left: (visible.min.x - screen.min.x).max(0.0),
            right: (screen.max.x - visible.max.x).max(0.0),
            bottom: (screen.max.y - visible.max.y).max(0.0),
        }
    }
}

/// What to ask of the window to bring it inside the work area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// The outer position to move to.
    pub position: Pos2,
    /// The inner size to resize to, where the window is larger than the area.
    pub inner_size: Option<Vec2>,
}

/// Tracks when the window needs a fit: after it opens and after it comes back
/// from minimized, maximized or full screen. A window the user drags partly
/// off screen later stays where the user puts it.
#[derive(Debug)]
pub struct WindowFit {
    wanted: bool,
    attempts: u8,
}

impl Default for WindowFit {
    fn default() -> Self {
        Self {
            wanted: true,
            attempts: 0,
        }
    }
}

impl WindowFit {
    /// The next request for this frame, from the state the window reports.
    ///
    /// `area` returns the work area for the outer rectangle it receives.
    pub fn plan(
        &mut self,
        info: &ViewportInfo,
        area: impl FnOnce(Rect) -> Option<Rect>,
    ) -> Option<Fit> {
        let away = [info.minimized, info.maximized, info.fullscreen]
            .into_iter()
            .any(|state| state == Some(true));
        if away {
            self.wanted = true;
            self.attempts = 0;
            return None;
        }
        if !self.wanted {
            return None;
        }
        let (outer, inner) = (info.outer_rect?, info.inner_rect?);
        let Some(area) = area(outer) else {
            self.wanted = false;
            return None;
        };
        let fitted = fit_into(outer, area);
        let moved = (fitted.min - outer.min).abs().max_elem() > TOLERANCE;
        let shrunk = (outer.size() - fitted.size()).max_elem() > TOLERANCE;
        if !moved && !shrunk {
            self.wanted = false;
            return None;
        }
        self.attempts += 1;
        if self.attempts >= MAXIMUM_ATTEMPTS {
            self.wanted = false;
        }
        let inner_size = shrunk.then(|| {
            let cut = outer.size() - fitted.size();
            (inner.size() - cut).max(Vec2::splat(1.0))
        });
        Some(Fit {
            position: fitted.min,
            inner_size,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rect(left: f32, top: f32, width: f32, height: f32) -> Rect {
        Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, height))
    }

    const WORK: fn() -> Rect = || rect(0.0, 0.0, 1920.0, 1040.0);

    #[test]
    fn a_window_inside_the_area_stays() {
        let outer = rect(100.0, 100.0, 1296.0, 839.0);
        assert_eq!(fit_into(outer, WORK()), outer);
    }

    #[test]
    fn a_window_past_the_bottom_edge_moves_up() {
        let outer = rect(234.0, 234.0, 1296.0, 839.0);
        let fitted = fit_into(outer, WORK());
        assert_eq!(fitted.size(), outer.size());
        assert_eq!(fitted.min, Pos2::new(234.0, 1040.0 - 839.0));
        assert!(fitted.max.y <= 1040.0);
    }

    #[test]
    fn a_window_past_the_right_edge_moves_left() {
        let outer = rect(1000.0, 50.0, 1296.0, 839.0);
        let fitted = fit_into(outer, WORK());
        assert_eq!(fitted.min, Pos2::new(1920.0 - 1296.0, 50.0));
    }

    #[test]
    fn a_negative_position_moves_to_the_corner_of_the_area() {
        let outer = rect(-300.0, -40.0, 800.0, 600.0);
        assert_eq!(fit_into(outer, WORK()).min, Pos2::ZERO);
    }

    #[test]
    fn a_window_larger_than_the_area_shrinks_to_it() {
        let area = rect(0.0, 0.0, 1366.0, 728.0);
        let outer = rect(234.0, 234.0, 1296.0, 839.0);
        let fitted = fit_into(outer, area);
        assert_eq!(fitted, rect(70.0, 0.0, 1296.0, 728.0));
        let wide = fit_into(rect(10.0, 10.0, 2000.0, 500.0), area);
        assert_eq!(wide, rect(0.0, 10.0, 1366.0, 500.0));
    }

    #[test]
    fn an_area_at_an_offset_origin_keeps_the_window_on_that_monitor() {
        let left_monitor = rect(-1920.0, -200.0, 1920.0, 1040.0);
        let outer = rect(-300.0, 500.0, 1296.0, 839.0);
        let fitted = fit_into(outer, left_monitor);
        assert_eq!(fitted.min, Pos2::new(-1296.0, 840.0 - 839.0));
        assert!(left_monitor.contains_rect(fitted));

        let right_monitor = rect(1920.0, 0.0, 1280.0, 984.0);
        let outer = rect(1800.0, 900.0, 800.0, 600.0);
        let fitted = fit_into(outer, right_monitor);
        assert_eq!(fitted.min, Pos2::new(1920.0, 384.0));
        assert!(right_monitor.contains_rect(fitted));
    }

    #[test]
    fn a_degenerate_area_changes_nothing() {
        let outer = rect(10.0, 10.0, 800.0, 600.0);
        assert_eq!(fit_into(outer, rect(0.0, 0.0, 0.0, 0.0)), outer);
        assert_eq!(
            fit_into(
                outer,
                Rect::from_min_max(Pos2::new(f32::NAN, 0.0), Pos2::new(1.0, 1.0))
            ),
            outer
        );
    }

    #[test]
    fn the_visible_client_is_the_part_inside_the_area() {
        let inner = rect(242.0, 273.0, 1280.0, 800.0);
        let shown = visible_client(inner, WORK()).unwrap();
        assert_eq!(shown, rect(0.0, 0.0, 1280.0, 1040.0 - 273.0));
        assert_eq!(
            Insets::between(rect(0.0, 0.0, 1280.0, 800.0), shown),
            Insets {
                left: 0.0,
                right: 0.0,
                bottom: 33.0,
            }
        );
    }

    #[test]
    fn the_visible_client_follows_a_window_past_the_left_and_right_edges() {
        let inner = rect(-100.0, 0.0, 1280.0, 800.0);
        let shown = visible_client(inner, WORK()).unwrap();
        assert_eq!(shown, rect(100.0, 0.0, 1180.0, 800.0));
        let inner = rect(1000.0, 0.0, 1280.0, 800.0);
        let shown = visible_client(inner, WORK()).unwrap();
        assert_eq!(shown, rect(0.0, 0.0, 920.0, 800.0));
        let insets = Insets::between(rect(0.0, 0.0, 1280.0, 800.0), shown);
        assert_eq!(
            insets,
            Insets {
                left: 0.0,
                right: 360.0,
                bottom: 0.0,
            }
        );
    }

    #[test]
    fn a_window_fully_outside_shows_nothing() {
        assert_eq!(
            visible_client(rect(3000.0, 0.0, 800.0, 600.0), WORK()),
            None
        );
    }

    fn info(outer: Rect, inner_margin: Vec2) -> ViewportInfo {
        ViewportInfo {
            outer_rect: Some(outer),
            inner_rect: Some(Rect::from_min_max(
                outer.min + inner_margin,
                outer.max - Vec2::new(inner_margin.x, 0.0),
            )),
            ..ViewportInfo::default()
        }
    }

    #[test]
    fn the_first_frame_moves_a_window_that_hangs_below_the_area() {
        let mut fit = WindowFit::default();
        let outer = rect(234.0, 234.0, 1296.0, 839.0);
        let state = info(outer, Vec2::new(8.0, 31.0));
        let step = fit.plan(&state, |_| Some(WORK())).unwrap();
        assert_eq!(step.position, Pos2::new(234.0, 201.0));
        assert_eq!(step.inner_size, None);
        let settled = info(
            Rect::from_min_size(step.position, outer.size()),
            Vec2::new(8.0, 31.0),
        );
        assert_eq!(fit.plan(&settled, |_| Some(WORK())), None);
        assert_eq!(fit.plan(&state, |_| Some(WORK())), None);
    }

    #[test]
    fn a_window_larger_than_the_area_is_resized_by_the_amount_cut() {
        let mut fit = WindowFit::default();
        let outer = rect(0.0, 0.0, 1296.0, 839.0);
        let state = info(outer, Vec2::new(8.0, 31.0));
        let area = rect(0.0, 0.0, 1366.0, 728.0);
        let step = fit.plan(&state, |_| Some(area)).unwrap();
        let inner = state.inner_rect.unwrap();
        assert_eq!(
            step.inner_size,
            Some(Vec2::new(inner.width(), inner.height() - (839.0 - 728.0)))
        );
    }

    #[test]
    fn a_restore_from_maximized_asks_for_a_new_fit() {
        let mut fit = WindowFit::default();
        let inside = info(rect(100.0, 100.0, 800.0, 600.0), Vec2::new(8.0, 31.0));
        assert_eq!(fit.plan(&inside, |_| Some(WORK())), None);
        let mut maximized = inside.clone();
        maximized.maximized = Some(true);
        assert_eq!(fit.plan(&maximized, |_| Some(WORK())), None);
        let hanging = info(rect(900.0, 700.0, 1296.0, 839.0), Vec2::new(8.0, 31.0));
        let step = fit.plan(&hanging, |_| Some(WORK())).unwrap();
        assert_eq!(step.position, Pos2::new(624.0, 201.0));
    }

    #[test]
    fn a_restore_from_minimized_asks_for_a_new_fit() {
        let mut fit = WindowFit::default();
        let inside = info(rect(100.0, 100.0, 800.0, 600.0), Vec2::new(8.0, 31.0));
        assert_eq!(fit.plan(&inside, |_| Some(WORK())), None);
        let minimized = ViewportInfo {
            minimized: Some(true),
            ..ViewportInfo::default()
        };
        assert_eq!(fit.plan(&minimized, |_| Some(WORK())), None);
        let hanging = info(rect(100.0, 900.0, 800.0, 600.0), Vec2::new(8.0, 31.0));
        assert!(fit.plan(&hanging, |_| Some(WORK())).is_some());
    }

    #[test]
    fn a_window_the_user_drags_off_screen_later_is_left_alone() {
        let mut fit = WindowFit::default();
        let inside = info(rect(100.0, 100.0, 800.0, 600.0), Vec2::new(8.0, 31.0));
        assert_eq!(fit.plan(&inside, |_| Some(WORK())), None);
        let dragged = info(rect(1800.0, 900.0, 800.0, 600.0), Vec2::new(8.0, 31.0));
        assert_eq!(fit.plan(&dragged, |_| Some(WORK())), None);
    }

    #[test]
    fn a_refused_size_does_not_repeat_without_end() {
        let mut fit = WindowFit::default();
        let state = info(rect(0.0, 0.0, 1296.0, 839.0), Vec2::new(8.0, 31.0));
        let area = rect(0.0, 0.0, 1366.0, 728.0);
        let sent = (0..20)
            .filter_map(|_| fit.plan(&state, |_| Some(area)))
            .count();
        assert_eq!(sent, usize::from(MAXIMUM_ATTEMPTS));
    }

    #[test]
    fn the_plan_waits_for_a_frame_that_reports_the_window() {
        let mut fit = WindowFit::default();
        let unknown = ViewportInfo::default();
        assert_eq!(fit.plan(&unknown, |_| Some(WORK())), None);
        let hanging = info(rect(234.0, 234.0, 1296.0, 839.0), Vec2::new(8.0, 31.0));
        assert!(fit.plan(&hanging, |_| Some(WORK())).is_some());
    }

    #[test]
    fn no_work_area_ends_the_wait_without_a_request() {
        let mut fit = WindowFit::default();
        let hanging = info(rect(234.0, 234.0, 1296.0, 839.0), Vec2::new(8.0, 31.0));
        assert_eq!(fit.plan(&hanging, |_| None), None);
        assert_eq!(fit.plan(&hanging, |_| Some(WORK())), None);
    }

    #[cfg(windows)]
    #[test]
    fn the_system_names_a_work_area_for_a_window_on_the_desktop() {
        let area = work_area_of(rect(0.0, 0.0, 400.0, 300.0), 1.0).unwrap();
        assert!(area.width() > 0.0 && area.height() > 0.0);
    }
}
