//! What the view reports about the pixel under the pointer.
//!
//! The readout names one point in the shared coordinate space and answers three
//! questions about it: what the left image holds there, what the right image
//! holds there, and what the comparison made of the pair. A point one image
//! does not cover reports nothing for that side, which is how a cropped image
//! and the image it was cropped from are told apart.

use crate::model::Placement;
use ca_image::compare::{Offset, PixelClass};
use ca_image::{ClassMask, RgbaImage};

/// One pixel, as the readout states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelDetails {
    /// The point, in the left image's coordinates.
    pub point: [i32; 2],
    /// The left image's pixel, when the left image covers the point.
    pub left: Option<[u8; 4]>,
    /// The right image's pixel, when the right image covers the point.
    pub right: Option<[u8; 4]>,
    /// What the comparison made of the point.
    pub verdict: Option<PixelClass>,
}

/// Where each buffer sits in the shared coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The left image, whose pixel (0, 0) defines the space.
    pub left: Placement,
    /// The right image, displaced by the comparison offset.
    pub right: Placement,
    /// The result buffer, which starts at or before the left image's corner.
    pub result: Placement,
}

impl Layout {
    /// The layout of one comparison.
    #[must_use]
    pub fn new(left: &RgbaImage, right: &RgbaImage, offset: Offset, origin: Offset) -> Self {
        Self {
            left: Placement::new([0, 0], [left.width(), left.height()]),
            right: Placement::new([offset.x, offset.y], [right.width(), right.height()]),
            result: Placement::new([origin.x, origin.y], [0, 0]),
        }
    }

    /// The layout with the result's size filled in.
    #[must_use]
    pub fn with_result(mut self, result: &RgbaImage, origin: Offset) -> Self {
        self.result = Placement::new([origin.x, origin.y], [result.width(), result.height()]);
        self
    }
}

/// Read one point out of the two images and the classification mask.
#[must_use]
pub fn sample(
    point: [i32; 2],
    layout: Layout,
    left: &RgbaImage,
    right: &RgbaImage,
    mask: &ClassMask,
) -> PixelDetails {
    let at = [f64_of(point[0]), f64_of(point[1])];
    let left_pixel = layout.left.pixel_of(at).and_then(|[x, y]| left.pixel(x, y));
    let right_pixel = layout
        .right
        .pixel_of(at)
        .and_then(|[x, y]| right.pixel(x, y));
    let verdict = layout.result.pixel_of(at).and_then(|[x, y]| mask.get(x, y));
    PixelDetails {
        point,
        left: left_pixel,
        right: right_pixel,
        verdict,
    }
}

/// A whole coordinate as the pixel-mapping arithmetic takes it. Adding a half
/// pixel names the middle of the pixel rather than its edge, which keeps the
/// floor inside the pixel the caller means.
#[allow(clippy::cast_precision_loss)]
fn f64_of(value: i32) -> f32 {
    value as f32 + 0.5
}

/// One pixel as four channel values.
#[must_use]
pub fn format_rgba(pixel: Option<[u8; 4]>) -> String {
    match pixel {
        Some([red, green, blue, alpha]) => {
            format!("{red}, {green}, {blue}, {alpha}")
        }
        None => "not covered".to_owned(),
    }
}

/// What one class is called in the readout.
#[must_use]
pub const fn verdict_label(verdict: Option<PixelClass>) -> &'static str {
    match verdict {
        Some(PixelClass::Same) => "match",
        Some(PixelClass::Similar) => "unimportant difference",
        Some(PixelClass::Different) => "important difference",
        Some(PixelClass::LeftOnly) => "left only",
        Some(PixelClass::RightOnly) => "right only",
        Some(PixelClass::Uncovered) => "not covered",
        None => "no result",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{format_rgba, sample, verdict_label, Layout};
    use ca_image::compare::{compare, CompareOptions, Offset, PixelClass};
    use ca_image::{NeverCancel, RgbaImage};

    fn pair() -> (RgbaImage, RgbaImage) {
        let mut left = RgbaImage::filled(4, 4, [10, 20, 30, 255]).unwrap();
        let right = RgbaImage::filled(4, 4, [10, 20, 30, 255]).unwrap();
        left.set_pixel(1, 1, [200, 0, 0, 255]);
        (left, right)
    }

    #[test]
    fn a_covered_point_reports_both_sides_and_the_verdict() {
        let (left, right) = pair();
        let options = CompareOptions::default();
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        let layout = Layout::new(&left, &right, Offset::zero(), result.origin)
            .with_result(&result.image, result.origin);
        let details = sample([1, 1], layout, &left, &right, &result.mask);
        assert_eq!(details.left, Some([200, 0, 0, 255]));
        assert_eq!(details.right, Some([10, 20, 30, 255]));
        assert_eq!(details.verdict, Some(PixelClass::Different));
        let same = sample([0, 0], layout, &left, &right, &result.mask);
        assert_eq!(same.verdict, Some(PixelClass::Same));
    }

    #[test]
    fn an_offset_moves_which_right_pixel_a_point_names() {
        let (left, right) = pair();
        let offset = Offset::new(2, 0);
        let options = CompareOptions {
            offset,
            ..CompareOptions::default()
        };
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        let layout = Layout::new(&left, &right, offset, result.origin)
            .with_result(&result.image, result.origin);
        // The right image now starts two pixels across, so the left edge is
        // covered by the left image alone.
        let edge = sample([0, 0], layout, &left, &right, &result.mask);
        assert!(edge.left.is_some());
        assert_eq!(edge.right, None);
        assert_eq!(edge.verdict, Some(PixelClass::LeftOnly));
        let overlap = sample([2, 0], layout, &left, &right, &result.mask);
        assert!(overlap.left.is_some() && overlap.right.is_some());
    }

    #[test]
    fn a_negative_offset_places_the_result_before_the_left_corner() {
        let (left, right) = pair();
        let offset = Offset::new(-2, -1);
        let options = CompareOptions {
            offset,
            ..CompareOptions::default()
        };
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        assert_eq!((result.origin.x, result.origin.y), (-2, -1));
        let layout = Layout::new(&left, &right, offset, result.origin)
            .with_result(&result.image, result.origin);
        let corner = sample([-2, -1], layout, &left, &right, &result.mask);
        assert_eq!(corner.left, None);
        assert!(corner.right.is_some());
        assert_eq!(corner.verdict, Some(PixelClass::RightOnly));
    }

    #[test]
    fn a_point_outside_everything_reports_nothing() {
        let (left, right) = pair();
        let options = CompareOptions::default();
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        let layout = Layout::new(&left, &right, Offset::zero(), result.origin)
            .with_result(&result.image, result.origin);
        let details = sample([99, 99], layout, &left, &right, &result.mask);
        assert_eq!(details.left, None);
        assert_eq!(details.right, None);
        assert_eq!(details.verdict, None);
        assert_eq!(format_rgba(details.left), "not covered");
        assert_eq!(verdict_label(details.verdict), "no result");
    }

    #[test]
    fn a_pixel_renders_as_four_channel_values() {
        assert_eq!(format_rgba(Some([1, 2, 3, 4])), "1, 2, 3, 4");
    }

    #[test]
    fn every_class_has_a_name() {
        for class in [
            PixelClass::Same,
            PixelClass::Similar,
            PixelClass::Different,
            PixelClass::LeftOnly,
            PixelClass::RightOnly,
            PixelClass::Uncovered,
        ] {
            assert!(!verdict_label(Some(class)).is_empty());
        }
    }
}
