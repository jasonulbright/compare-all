//! Pixel comparison of two RGBA8 buffers.
//!
//! One call produces three things: the rendered difference buffer for the
//! chosen display mode, a packed per-pixel classification mask, and the
//! counters in [`Totals`].
//!
//! The two images do not have to share a size. The right image carries an
//! offset, so a cropped image can be aligned with the one it was cropped from.
//! The result covers the union of the two placed rectangles; a pixel covered by
//! only one of them classifies as [`PixelClass::LeftOnly`] or
//! [`PixelClass::RightOnly`].

use crate::buffer::{try_zeroed, RgbaImage, BYTES_PER_PIXEL};
use crate::cancel::Cancel;
use crate::decode::Fidelity;
use crate::error::{Error, Result};
use crate::limits::Limits;
use crate::settings::{provisional, DisplayModeSetting, PictureViewSettings};
use rayon::prelude::*;

pub use crate::settings::ColorReplacement as Replacement;
pub use crate::settings::ToleranceColorSettings as ToleranceColors;

/// One side of a comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Side {
    /// The left image.
    #[default]
    Left,
    /// The right image.
    Right,
}

impl Side {
    /// The other side.
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// Displacement in pixels applied to the right image before comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Offset {
    /// Pixels to the right. Negative values move the image left.
    pub x: i32,
    /// Pixels down. Negative values move the image up.
    pub y: i32,
}

impl Offset {
    /// An offset from its two components.
    #[must_use]
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// The offset that aligns the two top-left corners.
    #[must_use]
    pub fn zero() -> Self {
        Self::default()
    }

    /// The offset moved by a step, saturating at the bounds of `i32`.
    #[must_use]
    pub fn nudged(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
        }
    }
}

/// How the result buffer renders a comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DisplayMode {
    /// Each pixel renders as a match, an unimportant difference or an
    /// important difference, tinted by [`ToleranceColors`] and shaded by the
    /// brightness of the source pixel.
    #[default]
    Tolerance,
    /// Matches render black; differences render yellow, brighter the larger
    /// the difference.
    MismatchRange,
    /// The two images combine by [`CompareOptions::blend_percent`], which is
    /// the weight of the left image.
    Blend,
    /// The side named by [`CompareOptions::side`] renders alone.
    SingleSide,
    /// Each channel renders as the absolute difference of the two sides.
    ChannelDifference,
    /// Each channel renders as the bitwise exclusive or of the two sides.
    ChannelXor,
}

impl DisplayMode {
    /// The mode a stored setting names, or `None` when the setting holds a
    /// value this build has no mode for.
    #[must_use]
    pub fn from_setting(setting: &DisplayModeSetting) -> Option<Self> {
        match setting {
            DisplayModeSetting::Tolerance => Some(DisplayMode::Tolerance),
            DisplayModeSetting::MismatchRange => Some(DisplayMode::MismatchRange),
            DisplayModeSetting::Blend => Some(DisplayMode::Blend),
            DisplayModeSetting::SingleSide => Some(DisplayMode::SingleSide),
            DisplayModeSetting::ChannelDifference => Some(DisplayMode::ChannelDifference),
            DisplayModeSetting::ChannelXor => Some(DisplayMode::ChannelXor),
            DisplayModeSetting::Unknown(_) => None,
        }
    }
}

/// What one pixel of the result was found to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelClass {
    /// Both sides cover the pixel and every compared channel is equal.
    Same,
    /// Both sides cover the pixel and the difference is at or below the
    /// tolerance, or a replacement rule covers it.
    Similar,
    /// Both sides cover the pixel and the difference is above the tolerance.
    Different,
    /// Only the left image covers the pixel.
    LeftOnly,
    /// Only the right image covers the pixel.
    RightOnly,
    /// Neither image covers the pixel. Such pixels exist because the result is
    /// the bounding box of the two placed images, which is larger than their
    /// union when one image sits beside the other.
    Uncovered,
}

impl PixelClass {
    /// The four-bit code stored in the mask.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            PixelClass::Same => 0,
            PixelClass::Similar => 1,
            PixelClass::Different => 2,
            PixelClass::LeftOnly => 3,
            PixelClass::RightOnly => 4,
            PixelClass::Uncovered => 5,
        }
    }

    /// The class a four-bit code names, or `None` for an unused code.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(PixelClass::Same),
            1 => Some(PixelClass::Similar),
            2 => Some(PixelClass::Different),
            3 => Some(PixelClass::LeftOnly),
            4 => Some(PixelClass::RightOnly),
            5 => Some(PixelClass::Uncovered),
            _ => None,
        }
    }

    /// True when the class counts as a difference.
    ///
    /// With `ignore_unimportant` set, a difference at or below the tolerance
    /// counts as a match, exactly as it renders.
    #[must_use]
    pub fn is_difference(self, ignore_unimportant: bool) -> bool {
        match self {
            PixelClass::Same | PixelClass::Uncovered => false,
            PixelClass::Similar => !ignore_unimportant,
            PixelClass::Different | PixelClass::LeftOnly | PixelClass::RightOnly => true,
        }
    }
}

/// The classification of every result pixel, two pixels to the byte.
///
/// The low nibble of a byte holds the even column, the high nibble the odd
/// one. Rows start on a byte boundary, so a row is `stride` bytes wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassMask {
    width: u32,
    height: u32,
    stride: usize,
    data: Vec<u8>,
}

impl ClassMask {
    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes in one row.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// The packed bytes, row major.
    #[must_use]
    pub fn packed(&self) -> &[u8] {
        &self.data
    }

    /// The class of one pixel, or `None` outside the mask or for a code this
    /// build does not name.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> Option<PixelClass> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let x = x as usize;
        let byte = *self.data.get(y as usize * self.stride + x / 2)?;
        let code = if x.is_multiple_of(2) {
            byte & 0x0F
        } else {
            byte >> 4
        };
        PixelClass::from_code(code)
    }
}

/// Counters over every pixel of the result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Pixels equal on both sides.
    pub same: u64,
    /// Pixels differing by at most the tolerance, or covered by a replacement.
    pub similar: u64,
    /// Pixels differing by more than the tolerance.
    pub different: u64,
    /// Pixels only the left image covers.
    pub left_only: u64,
    /// Pixels only the right image covers.
    pub right_only: u64,
    /// Pixels neither image covers.
    pub uncovered: u64,
    /// True when both sources stored more than eight bits per channel and the
    /// comparison found no difference. The two files are then equal at eight
    /// bits per channel, which is the precision the comparison runs at, and may
    /// still differ in the bits the decode dropped.
    pub equal_at_eight_bits: bool,
}

impl Totals {
    /// Every pixel of the result, including the ones neither image covers.
    #[must_use]
    pub fn total(self) -> u64 {
        self.compared_total().saturating_add(self.uncovered)
    }

    /// Pixels at least one image covers. The percentages are shares of this.
    #[must_use]
    pub fn compared_total(self) -> u64 {
        self.same
            .saturating_add(self.similar)
            .saturating_add(self.different)
            .saturating_add(self.left_only)
            .saturating_add(self.right_only)
    }

    /// Pixels that count as differences.
    #[must_use]
    pub fn difference_count(self, ignore_unimportant: bool) -> u64 {
        let mut count = self
            .different
            .saturating_add(self.left_only)
            .saturating_add(self.right_only);
        if !ignore_unimportant {
            count = count.saturating_add(self.similar);
        }
        count
    }

    /// Share of the result that counts as a difference, as a percentage.
    #[must_use]
    pub fn percent_different(self, ignore_unimportant: bool) -> f64 {
        ratio(
            self.difference_count(ignore_unimportant),
            self.compared_total(),
        )
    }

    /// Share of the result that counts as a match, as a percentage.
    #[must_use]
    pub fn percent_same(self, ignore_unimportant: bool) -> f64 {
        let total = self.compared_total();
        ratio(
            total.saturating_sub(self.difference_count(ignore_unimportant)),
            total,
        )
    }

    /// True when nothing counts as a difference.
    #[must_use]
    pub fn is_identical(self, ignore_unimportant: bool) -> bool {
        self.difference_count(ignore_unimportant) == 0
    }

    fn merge(self, other: Self) -> Self {
        Self {
            same: self.same.saturating_add(other.same),
            similar: self.similar.saturating_add(other.similar),
            different: self.different.saturating_add(other.different),
            left_only: self.left_only.saturating_add(other.left_only),
            right_only: self.right_only.saturating_add(other.right_only),
            uncovered: self.uncovered.saturating_add(other.uncovered),
            equal_at_eight_bits: self.equal_at_eight_bits || other.equal_at_eight_bits,
        }
    }

    fn add(&mut self, class: PixelClass) {
        match class {
            PixelClass::Same => self.same = self.same.saturating_add(1),
            PixelClass::Similar => self.similar = self.similar.saturating_add(1),
            PixelClass::Different => self.different = self.different.saturating_add(1),
            PixelClass::LeftOnly => self.left_only = self.left_only.saturating_add(1),
            PixelClass::RightOnly => self.right_only = self.right_only.saturating_add(1),
            PixelClass::Uncovered => self.uncovered = self.uncovered.saturating_add(1),
        }
    }
}

/// The counted share, as a percentage. The conversion to `f64` is exact for
/// every pixel count an image under the decode limits can reach.
#[allow(clippy::cast_precision_loss)]
fn ratio(part: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    part as f64 * 100.0 / total as f64
}

/// Everything one comparison needs beside the two images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareOptions {
    /// How the result buffer renders.
    pub mode: DisplayMode,
    /// Greatest per-channel difference still treated as unimportant.
    pub tolerance: u8,
    /// Whether unimportant differences render and count as matches.
    pub ignore_unimportant: bool,
    /// Weight of the left image in [`DisplayMode::Blend`], as a percentage
    /// from 0 to 100.
    pub blend_percent: u8,
    /// Side rendered by [`DisplayMode::SingleSide`].
    pub side: Side,
    /// Displacement applied to the right image.
    pub offset: Offset,
    /// Whether the alpha channel is left out of the comparison.
    pub ignore_alpha: bool,
    /// Whether two pixels that are fully transparent on both sides count as
    /// equal. A fully transparent pixel shows nothing, so the color channels it
    /// hides do not reach the viewer.
    pub transparent_pixels_equal: bool,
    /// Color substitutions treated as unimportant.
    pub replacements: Vec<Replacement>,
    /// Tints used by [`DisplayMode::Tolerance`].
    pub colors: ToleranceColors,
    /// Bounds applied to the size of the result. They stand above the decode
    /// bounds, because the result covers the bounding box of two placed images.
    pub result_limits: Limits,
    /// What the decode of the left image dropped on the way to RGBA8.
    pub left_fidelity: Fidelity,
    /// What the decode of the right image dropped on the way to RGBA8.
    pub right_fidelity: Fidelity,
}

impl Default for CompareOptions {
    fn default() -> Self {
        let view = PictureViewSettings::default();
        Self {
            mode: DisplayMode::Tolerance,
            tolerance: view.tolerance,
            ignore_unimportant: view.ignore_unimportant,
            blend_percent: view.blend_percent,
            side: Side::Left,
            offset: Offset::zero(),
            ignore_alpha: false,
            transparent_pixels_equal: provisional::TRANSPARENT_PIXELS_EQUAL,
            replacements: Vec::new(),
            colors: view.colors,
            result_limits: Limits::for_result(),
            left_fidelity: Fidelity::default(),
            right_fidelity: Fidelity::default(),
        }
    }
}

impl CompareOptions {
    /// Options built from stored view settings and stored replacements.
    ///
    /// # Errors
    /// Returns [`Error::OutOfRange`] when the settings name a display mode
    /// this build has no renderer for, or a blend percentage above 100.
    pub fn from_settings(view: &PictureViewSettings, replacements: &[Replacement]) -> Result<Self> {
        let mode = DisplayMode::from_setting(&view.mode).ok_or_else(|| {
            Error::OutOfRange("the stored display mode is not one this build renders".to_owned())
        })?;
        if view.blend_percent > 100 {
            return Err(Error::OutOfRange(
                "the blend percentage is above 100".to_owned(),
            ));
        }
        Ok(Self {
            mode,
            tolerance: view.tolerance,
            ignore_unimportant: view.ignore_unimportant,
            blend_percent: view.blend_percent,
            side: Side::Left,
            offset: Offset::new(view.offset_x, view.offset_y),
            ignore_alpha: false,
            transparent_pixels_equal: provisional::TRANSPARENT_PIXELS_EQUAL,
            replacements: replacements.to_vec(),
            colors: view.colors.clone(),
            result_limits: Limits::for_result(),
            left_fidelity: Fidelity::default(),
            right_fidelity: Fidelity::default(),
        })
    }
}

/// The result of one comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareResult {
    /// The rendered difference buffer for the chosen mode.
    pub image: RgbaImage,
    /// The class of every pixel of that buffer.
    pub mask: ClassMask,
    /// The counters over that buffer.
    pub totals: Totals,
    /// Position of the result's top-left corner in the left image's
    /// coordinates. It is zero or negative on each axis.
    pub origin: Offset,
    /// What the decode of the left image dropped on the way to RGBA8.
    pub left_fidelity: Fidelity,
    /// What the decode of the right image dropped on the way to RGBA8.
    pub right_fidelity: Fidelity,
}

/// Geometry of one comparison, in result coordinates.
///
/// The four start values are non-negative by construction: the result origin
/// is the smaller of zero and the offset on each axis, so subtracting it from
/// either image's position cannot go below zero. Every index built from them
/// below is therefore a `usize` addition inside a row that was proven long
/// enough.
struct Plan<'a> {
    left: &'a RgbaImage,
    right: &'a RgbaImage,
    options: &'a CompareOptions,
    width: usize,
    left_x0: usize,
    left_y0: usize,
    right_x0: usize,
    right_y0: usize,
}

impl Plan<'_> {
    /// The left pixels covering result row `cy`, if the row crosses the left
    /// image.
    fn left_row(&self, cy: usize) -> Option<&[u8]> {
        let y = cy.checked_sub(self.left_y0)?;
        self.left.row(u32::try_from(y).ok()?)
    }

    fn right_row(&self, cy: usize) -> Option<&[u8]> {
        let y = cy.checked_sub(self.right_y0)?;
        self.right.row(u32::try_from(y).ok()?)
    }

    fn row(&self, cy: usize, out_row: &mut [u8], mask_row: &mut [u8]) -> Totals {
        let left_row = self.left_row(cy);
        let right_row = self.right_row(cy);
        let mut totals = Totals::default();
        for cx in 0..self.width {
            let left = left_row.and_then(|row| sample(row, cx, self.left_x0));
            let right = right_row.and_then(|row| sample(row, cx, self.right_x0));
            let class = classify(left, right, self.options);
            let color = render(class, left, right, self.options);
            totals.add(class);
            if let Some(slot) = out_row.get_mut(cx * BYTES_PER_PIXEL..(cx + 1) * BYTES_PER_PIXEL) {
                slot.copy_from_slice(&color);
            }
            if let Some(byte) = mask_row.get_mut(cx / 2) {
                let code = class.code();
                if cx.is_multiple_of(2) {
                    *byte = (*byte & 0xF0) | code;
                } else {
                    *byte = (*byte & 0x0F) | (code << 4);
                }
            }
        }
        totals
    }
}

/// The pixel of `row` covering result column `cx`, or `None` when the column
/// lies outside the image the row belongs to.
fn sample(row: &[u8], cx: usize, x0: usize) -> Option<[u8; 4]> {
    let x = cx.checked_sub(x0)?;
    let start = x.checked_mul(BYTES_PER_PIXEL)?;
    let slice = row.get(start..start.checked_add(BYTES_PER_PIXEL)?)?;
    Some([slice[0], slice[1], slice[2], slice[3]])
}

/// Greatest per-channel difference of two pixels.
fn channel_distance(left: [u8; 4], right: [u8; 4], ignore_alpha: bool) -> u8 {
    let channels = if ignore_alpha { 3 } else { 4 };
    let mut worst = 0u8;
    for index in 0..channels {
        let distance = left[index].abs_diff(right[index]);
        if distance > worst {
            worst = distance;
        }
    }
    worst
}

fn colors_equal(left: [u8; 4], right: [u8; 4], ignore_alpha: bool) -> bool {
    channel_distance(left, right, ignore_alpha) == 0
}

/// True when both pixels are fully transparent and the option treats that as
/// equal. The color channels of a fully transparent pixel never reach the
/// viewer, so they cannot be seen to differ.
fn both_invisible(left: [u8; 4], right: [u8; 4], options: &CompareOptions) -> bool {
    options.transparent_pixels_equal && left[3] == 0 && right[3] == 0
}

fn classify(left: Option<[u8; 4]>, right: Option<[u8; 4]>, options: &CompareOptions) -> PixelClass {
    match (left, right) {
        (Some(left), Some(right)) => {
            if both_invisible(left, right, options) {
                return PixelClass::Same;
            }
            let distance = channel_distance(left, right, options.ignore_alpha);
            if distance == 0 {
                return PixelClass::Same;
            }
            if distance <= options.tolerance {
                return PixelClass::Similar;
            }
            let replaced = options.replacements.iter().any(|rule| {
                colors_equal(left, rule.matched, options.ignore_alpha)
                    && colors_equal(right, rule.replacement, options.ignore_alpha)
            });
            if replaced {
                PixelClass::Similar
            } else {
                PixelClass::Different
            }
        }
        (Some(_), None) => PixelClass::LeftOnly,
        (None, Some(_)) => PixelClass::RightOnly,
        (None, None) => PixelClass::Uncovered,
    }
}

/// Perceived brightness of a pixel, ignoring alpha.
fn luminance(pixel: [u8; 4]) -> u8 {
    let value =
        (u32::from(pixel[0]) * 77 + u32::from(pixel[1]) * 150 + u32::from(pixel[2]) * 29) >> 8;
    // The weights sum to 256, so the shifted value is at most 255.
    u8::try_from(value).unwrap_or(u8::MAX)
}

fn tint(color: [u8; 3], intensity: u8) -> [u8; 4] {
    let scale = |channel: u8| -> u8 {
        let value = u32::from(channel) * u32::from(intensity) / 255;
        u8::try_from(value).unwrap_or(u8::MAX)
    };
    [scale(color[0]), scale(color[1]), scale(color[2]), u8::MAX]
}

fn blend(left: [u8; 4], right: [u8; 4], percent: u8) -> [u8; 4] {
    let weight = u32::from(percent.min(100));
    let mix = |a: u8, b: u8| -> u8 {
        let value = (u32::from(a) * weight + u32::from(b) * (100 - weight)) / 100;
        u8::try_from(value).unwrap_or(u8::MAX)
    };
    [
        mix(left[0], right[0]),
        mix(left[1], right[1]),
        mix(left[2], right[2]),
        mix(left[3], right[3]),
    ]
}

const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

fn render(
    class: PixelClass,
    left: Option<[u8; 4]>,
    right: Option<[u8; 4]>,
    options: &CompareOptions,
) -> [u8; 4] {
    if class == PixelClass::Uncovered {
        return TRANSPARENT;
    }
    match options.mode {
        DisplayMode::Tolerance => {
            let source = left.or(right).unwrap_or(TRANSPARENT);
            let color = match class {
                PixelClass::Same => options.colors.same,
                PixelClass::Similar if options.ignore_unimportant => options.colors.same,
                PixelClass::Similar => options.colors.similar,
                PixelClass::Different
                | PixelClass::LeftOnly
                | PixelClass::RightOnly
                | PixelClass::Uncovered => options.colors.different,
            };
            tint(color, luminance(source))
        }
        DisplayMode::MismatchRange => {
            let magnitude = match (left, right) {
                (Some(left), Some(right)) => {
                    let counted = match class {
                        PixelClass::Same => false,
                        PixelClass::Similar => !options.ignore_unimportant,
                        _ => true,
                    };
                    if counted {
                        channel_distance(left, right, options.ignore_alpha)
                    } else {
                        0
                    }
                }
                _ => u8::MAX,
            };
            [magnitude, magnitude, 0, u8::MAX]
        }
        DisplayMode::Blend => match (left, right) {
            (Some(left), Some(right)) => blend(left, right, options.blend_percent),
            (Some(only), None) | (None, Some(only)) => only,
            (None, None) => TRANSPARENT,
        },
        DisplayMode::SingleSide => match options.side {
            Side::Left => left.unwrap_or(TRANSPARENT),
            Side::Right => right.unwrap_or(TRANSPARENT),
        },
        DisplayMode::ChannelDifference => {
            let left = left.unwrap_or(TRANSPARENT);
            let right = right.unwrap_or(TRANSPARENT);
            [
                left[0].abs_diff(right[0]),
                left[1].abs_diff(right[1]),
                left[2].abs_diff(right[2]),
                if options.ignore_alpha {
                    u8::MAX
                } else {
                    left[3].abs_diff(right[3])
                },
            ]
        }
        DisplayMode::ChannelXor => {
            let left = left.unwrap_or(TRANSPARENT);
            let right = right.unwrap_or(TRANSPARENT);
            [
                left[0] ^ right[0],
                left[1] ^ right[1],
                left[2] ^ right[2],
                if options.ignore_alpha {
                    u8::MAX
                } else {
                    left[3] ^ right[3]
                },
            ]
        }
    }
}

/// Compares two images and renders the result.
///
/// The work runs on the rayon pool, one row band per row. One row band is the
/// cancellation latency: `cancel` is polled before each band, so a fired signal
/// stops the comparison after at most the bands already in flight, never after
/// the whole image. Memory use is the result buffer plus the mask, which is one
/// nibble per result pixel.
///
/// # Errors
/// Returns [`Error::OutOfRange`] for a blend percentage above 100,
/// [`Error::Cancelled`] when `cancel` fires, and [`Error::ResultTooLarge`] when
/// the bounding box of the two placed images crosses
/// [`CompareOptions::result_limits`].
pub fn compare<C: Cancel>(
    left: &RgbaImage,
    right: &RgbaImage,
    options: &CompareOptions,
    cancel: &C,
) -> Result<CompareResult> {
    if options.blend_percent > 100 {
        return Err(Error::OutOfRange(
            "the blend percentage is above 100".to_owned(),
        ));
    }

    let offset_x = i64::from(options.offset.x);
    let offset_y = i64::from(options.offset.y);
    let origin_x = offset_x.min(0);
    let origin_y = offset_y.min(0);
    let end_x = i64::from(left.width()).max(offset_x + i64::from(right.width()));
    let end_y = i64::from(left.height()).max(offset_y + i64::from(right.height()));

    let width = span(end_x - origin_x)?;
    let height = span(end_y - origin_y)?;
    options.result_limits.check_result(width, height)?;

    let plan = Plan {
        left,
        right,
        options,
        width: width as usize,
        left_x0: index_from(-origin_x)?,
        left_y0: index_from(-origin_y)?,
        right_x0: index_from(offset_x - origin_x)?,
        right_y0: index_from(offset_y - origin_y)?,
    };

    let mask_stride = (width as usize).div_ceil(2);
    let mask_len = mask_stride
        .checked_mul(height as usize)
        .ok_or(Error::ResultTooLarge {
            width,
            height,
            limit_pixels: options.result_limits.max_pixels,
            limit_bytes: options.result_limits.max_decoded_bytes,
        })?;

    let mut image = RgbaImage::new_within(width, height, &options.result_limits)?;
    let mut mask_data = try_zeroed(mask_len)?;
    let out_stride = image.row_stride();

    let mut totals = image
        .pixels_mut()
        .par_chunks_mut(out_stride)
        .zip(mask_data.par_chunks_mut(mask_stride))
        .enumerate()
        .map(|(cy, (out_row, mask_row))| {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Ok(plan.row(cy, out_row, mask_row))
        })
        .try_reduce(Totals::default, |a, b| Ok(a.merge(b)))?;

    totals.equal_at_eight_bits = (options.left_fidelity.precision_reduced
        || options.right_fidelity.precision_reduced)
        && totals.is_identical(options.ignore_unimportant);

    Ok(CompareResult {
        image,
        mask: ClassMask {
            width,
            height,
            stride: mask_stride,
            data: mask_data,
        },
        totals,
        origin: Offset::new(
            i32::try_from(origin_x).unwrap_or(i32::MIN),
            i32::try_from(origin_y).unwrap_or(i32::MIN),
        ),
        left_fidelity: options.left_fidelity,
        right_fidelity: options.right_fidelity,
    })
}

/// A result dimension, rejected when it does not fit in `u32`.
fn span(value: i64) -> Result<u32> {
    if value <= 0 {
        return Err(Error::EmptyImage);
    }
    u32::try_from(value).map_err(|_| Error::TooLarge {
        needed: u64::try_from(value).unwrap_or(u64::MAX),
        limit: u64::from(u32::MAX),
        unit: "pixels",
    })
}

/// A non-negative offset converted to an index.
fn index_from(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::OutOfRange("the offset is out of range".to_owned()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cancel::NeverCancel;

    fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
        RgbaImage::filled(width, height, color).unwrap()
    }

    #[test]
    fn equal_images_classify_as_matches() {
        let left = solid(2, 2, [10, 20, 30, 255]);
        let right = solid(2, 2, [10, 20, 30, 255]);
        let options = CompareOptions::default();
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        assert_eq!(result.totals.same, 4);
        assert_eq!(result.totals.different, 0);
        assert!(result.totals.is_identical(false));
        assert_eq!(result.mask.get(1, 1), Some(PixelClass::Same));
    }

    #[test]
    fn a_class_code_round_trips() {
        for class in [
            PixelClass::Same,
            PixelClass::Similar,
            PixelClass::Different,
            PixelClass::LeftOnly,
            PixelClass::RightOnly,
            PixelClass::Uncovered,
        ] {
            assert_eq!(PixelClass::from_code(class.code()), Some(class));
        }
        assert_eq!(PixelClass::from_code(15), None);
    }

    #[test]
    fn a_blend_percentage_above_one_hundred_is_rejected() {
        let left = solid(1, 1, [0, 0, 0, 255]);
        let options = CompareOptions {
            blend_percent: 101,
            ..CompareOptions::default()
        };
        let error = compare(&left, &left, &options, &NeverCancel);
        assert!(matches!(error, Err(Error::OutOfRange(_))));
    }
}
