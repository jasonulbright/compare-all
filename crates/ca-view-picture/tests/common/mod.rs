//! Images and frames the picture view tests are built from.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use ca_ui::testing::{context, sized_input};
use ca_ui::view::SessionView;
use ca_view_picture::model::Pane;
use ca_view_picture::{PictureView, WidgetRect};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a test waits for a comparison to reach the view.
pub const BUDGET: Duration = Duration::from_secs(20);

/// Write a solid image with one optional differing pixel.
pub fn write_png(path: &Path, width: u32, height: u32, color: [u8; 4], mark: Option<(u32, u32)>) {
    let mut buffer = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    if let Some((x, y)) = mark {
        buffer.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
    }
    buffer.save(path).expect("the fixture image is written");
}

/// A pair of small images that differ in one pixel.
pub fn pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("left.png");
    let right = dir.join("right.png");
    write_png(&left, 8, 6, [10, 20, 30, 255], Some((3, 2)));
    write_png(&right, 8, 6, [10, 20, 30, 255], None);
    (left, right)
}

/// Write a white image with a blue rectangle and a filled circle.
///
/// The two shapes sit apart, so a pane that shows only part of the image shows
/// only one of them.
pub fn write_shapes(path: &Path, circle: [u8; 4]) {
    let mut buffer = image::RgbaImage::from_pixel(160, 100, image::Rgba([255, 255, 255, 255]));
    for y in 20..70 {
        for x in 20..80 {
            buffer.put_pixel(x, y, image::Rgba([0, 0, 200, 255]));
        }
    }
    for y in 40..70 {
        for x in 100..130 {
            let dx = f64::from(x) - 114.5;
            let dy = f64::from(y) - 54.5;
            if dx.mul_add(dx, dy * dy) <= 225.0 {
                buffer.put_pixel(x, y, image::Rgba(circle));
            }
        }
    }
    buffer.save(path).expect("the fixture image is written");
}

/// A pair of shape images whose circles differ in color.
pub fn shapes(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("shapes-left.png");
    let right = dir.join("shapes-right.png");
    write_shapes(&left, [255, 165, 0, 255]);
    write_shapes(&right, [255, 69, 0, 255]);
    (left, right)
}

/// Where one frame drew everything the view reports.
pub struct Geometry {
    /// The window the frame ran in.
    pub window: egui::Rect,
    /// The rectangle of every pane that was laid out.
    pub panes: Vec<(Pane, egui::Rect)>,
    /// The rectangle the content was painted in, per pane.
    pub images: Vec<(Pane, egui::Rect)>,
    /// Every rectangle the view reports for a layout check.
    pub widgets: Vec<WidgetRect>,
    /// The status bar.
    pub status: egui::Rect,
    /// The toolbar.
    pub toolbar: egui::Rect,
    /// Lines the toolbar wrapped onto.
    pub toolbar_rows: usize,
    /// True while the magnification follows the pane size.
    pub fitted: bool,
}

impl Geometry {
    /// The rectangle of one pane.
    pub fn pane(&self, pane: Pane) -> egui::Rect {
        self.panes
            .iter()
            .find(|(each, _)| *each == pane)
            .map(|(_, rect)| *rect)
            .expect("the pane was laid out")
    }

    /// The rectangle one pane's content was painted in.
    pub fn image(&self, pane: Pane) -> egui::Rect {
        self.images
            .iter()
            .find(|(each, _)| *each == pane)
            .map(|(_, rect)| *rect)
            .expect("the pane painted its content")
    }
}

/// How far a rectangle may fall outside the one that holds it.
const SLACK: f32 = 1.0;

/// Run one frame at a window size and check what it drew.
///
/// Every rectangle must lie inside the window, every painted image inside its
/// own pane, and no two reported widgets may overlap.
pub fn layout(view: &mut PictureView, ctx: &egui::Context, width: f32, height: f32) -> Geometry {
    frame(view, ctx, width, height);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
    let panes: Vec<(Pane, egui::Rect)> = Pane::ALL
        .into_iter()
        .filter_map(|pane| view.pane_rect(pane).map(|rect| (pane, rect)))
        .collect();
    let images: Vec<(Pane, egui::Rect)> = Pane::ALL
        .into_iter()
        .filter_map(|pane| view.image_rect(pane).map(|rect| (pane, rect)))
        .collect();
    let widgets: Vec<WidgetRect> = view.widget_rects().to_vec();
    let geometry = Geometry {
        window,
        panes,
        images,
        widgets,
        status: view.status_rect(),
        toolbar: view.toolbar_rect(),
        toolbar_rows: view.toolbar_rows(),
        fitted: view.fits_to_panes(),
    };

    let holds = |outer: egui::Rect, inner: egui::Rect| outer.expand(SLACK).contains_rect(inner);
    assert!(
        holds(window, geometry.status),
        "the status bar {:?} runs outside the window {window:?}",
        geometry.status
    );
    assert!(
        holds(window, geometry.toolbar),
        "the toolbar {:?} runs outside the window {window:?}",
        geometry.toolbar
    );
    for (pane, rect) in &geometry.panes {
        assert!(
            holds(window, *rect),
            "{} runs outside the window",
            pane.label()
        );
    }
    // A magnified image is larger than its pane on purpose and is clipped, so
    // only a fitted image has to lie inside.
    if geometry.fitted {
        for (pane, rect) in &geometry.images {
            assert!(
                holds(geometry.pane(*pane), *rect),
                "the image of {} runs outside its pane",
                pane.label()
            );
        }
    }
    for widget in &geometry.widgets {
        assert!(
            holds(window, widget.rect),
            "{} runs outside the window",
            widget.name
        );
        if let Some(pane) = widget.pane {
            assert!(
                holds(geometry.pane(pane), widget.rect),
                "{} runs outside {}",
                widget.name,
                pane.label()
            );
        }
    }
    for (index, one) in geometry.widgets.iter().enumerate() {
        for other in geometry.widgets.iter().skip(index + 1) {
            assert!(
                !one.rect.shrink(0.5).intersects(other.rect.shrink(0.5)),
                "{} covers {}",
                other.name,
                one.name
            );
        }
    }
    geometry
}

/// A view over two paths, with its own egui context.
pub fn open(left: PathBuf, right: PathBuf, instance: u64) -> (PictureView, egui::Context) {
    (
        PictureView::new(left, right, &context(), instance),
        egui::Context::default(),
    )
}

/// Run one frame of the view at the stated window size.
pub fn frame(view: &mut PictureView, ctx: &egui::Context, width: f32, height: f32) {
    frame_with(view, ctx, width, height, Vec::new());
}

/// Run one frame carrying `events`.
pub fn frame_with(
    view: &mut PictureView,
    ctx: &egui::Context,
    width: f32,
    height: f32,
    events: Vec<egui::Event>,
) {
    view.tick();
    let input = egui::RawInput {
        events,
        ..sized_input(width, height)
    };
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            view.ui(ui, &context());
        });
    });
}

/// Run frames until `condition` holds or the budget runs out.
pub fn run_until(
    view: &mut PictureView,
    ctx: &egui::Context,
    mut condition: impl FnMut(&PictureView) -> bool,
) -> bool {
    let deadline = Instant::now() + BUDGET;
    loop {
        frame(view, ctx, 1_280.0, 800.0);
        if condition(view) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Run frames until a comparison is on screen.
pub fn run_until_ready(view: &mut PictureView, ctx: &egui::Context) -> bool {
    run_until(view, ctx, SessionView::is_ready)
}

/// Run frames of a stated size until a comparison is on screen.
pub fn run_until_ready_at(
    view: &mut PictureView,
    ctx: &egui::Context,
    width: f32,
    height: f32,
) -> bool {
    let deadline = Instant::now() + BUDGET;
    loop {
        frame(view, ctx, width, height);
        if view.is_ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}
