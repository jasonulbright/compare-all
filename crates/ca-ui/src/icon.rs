//! The window icon, drawn in code rather than loaded from a file.
//!
//! Two overlapping page shapes, rasterised into straight red green blue alpha
//! bytes. Drawing it here keeps the binary free of an image decoder and free of
//! a file that has to be found at run time.

use crate::theme::icon as colors;

/// Edge length of the icon, in pixels.
const SIZE: u32 = 64;
/// Where the back page's top left corner sits.
const BACK: Page = Page {
    left: 8,
    top: 6,
    width: 32,
    height: 44,
    fill: colors::BACK_FILL,
    edge: colors::BACK_EDGE,
};
/// Where the front page sits, overlapping the back one.
const FRONT: Page = Page {
    left: 24,
    top: 14,
    width: 32,
    height: 44,
    fill: colors::FRONT_FILL,
    edge: colors::FRONT_EDGE,
};

/// One page shape.
struct Page {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    fill: [u8; 4],
    edge: [u8; 4],
}

impl Page {
    /// The color this page contributes at a pixel, if it covers it.
    ///
    /// The top right corner is cut away on the diagonal, which is what makes
    /// the shape read as a page rather than a rectangle.
    const fn sample(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x < self.left || y < self.top {
            return None;
        }
        let (dx, dy) = (x - self.left, y - self.top);
        if dx >= self.width || dy >= self.height {
            return None;
        }
        let fold = self.width / 3;
        if dy < fold && dx + fold >= self.width + dy {
            return None;
        }
        let on_fold = dy < fold && dx + fold + 1 >= self.width + dy;
        let on_edge = dx == 0 || dy == 0 || dx + 1 == self.width || dy + 1 == self.height;
        if on_edge || on_fold {
            Some(self.edge)
        } else {
            Some(self.fill)
        }
    }
}

/// The application icon as raw red green blue alpha bytes.
#[must_use]
pub fn application_icon() -> egui::IconData {
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            // The front page is drawn over the back one, so it is asked first.
            let Some(color) = FRONT.sample(x, y).or_else(|| BACK.sample(x, y)) else {
                continue;
            };
            let offset = ((y * SIZE + x) * 4) as usize;
            if let Some(pixel) = rgba.get_mut(offset..offset + 4) {
                pixel.copy_from_slice(&color);
            }
        }
    }
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::{application_icon, SIZE};

    #[test]
    fn the_icon_is_a_full_square_of_pixels() {
        let icon = application_icon();
        assert_eq!(icon.width, SIZE);
        assert_eq!(icon.height, SIZE);
        assert_eq!(icon.rgba.len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn the_icon_has_something_drawn_on_it_and_something_left_clear() {
        let icon = application_icon();
        let opaque = icon
            .rgba
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0)
            .count();
        assert!(opaque > 1_000, "only {opaque} pixels were drawn");
        assert!(
            opaque < (SIZE * SIZE) as usize,
            "the icon fills its whole square"
        );
    }

    #[test]
    fn the_two_pages_overlap() {
        let icon = application_icon();
        // A pixel inside both page rectangles takes the front page's fill.
        let (x, y) = (30u32, 30u32);
        let offset = ((y * SIZE + x) * 4) as usize;
        assert_eq!(&icon.rgba[offset..offset + 3], &[0xF2, 0xF4, 0xF8]);
        // A pixel inside the back page only keeps the back page's fill.
        let (x, y) = (14u32, 30u32);
        let offset = ((y * SIZE + x) * 4) as usize;
        assert_eq!(&icon.rgba[offset..offset + 3], &[0x9A, 0xA6, 0xB8]);
    }
}
