//! The background work behind a picture comparison: reading both files,
//! decoding them under the configured limits, aligning them and comparing them.
//!
//! Everything in this module runs on a worker thread. The frame thread never
//! reads a file, never decodes, never compares and never resamples; it paints
//! what a run posted and hands finished pixels to the graphics device.
//!
//! Cancellation is polled between the steps and inside the comparison itself,
//! so a run that has been superseded stops without finishing the work nobody
//! will look at.

use ca_image::compare::{
    CompareOptions, CompareResult, DisplayMode, Offset, Side, ToleranceColors,
};
use ca_image::decode::{DecodeOptions, Fidelity, Metadata, SourceFormat};
use ca_image::transform::{self, Filter, Rotation};
use ca_image::{ClassMask, Error, Limits, RgbaImage, Totals};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Longest side of the reduced copy a pane draws when the magnification makes
/// the full buffer unnecessary.
///
/// Reduced copies let large panes be shown whole without handing every
/// full-resolution pixel to the device.
pub const PREVIEW_MAX_SIDE: u32 = 2_048;

/// Quarter turns and reflections applied to one side before it is compared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SideTransform {
    /// Quarter turns clockwise, counted modulo four.
    pub quarter_turns: u8,
    /// Reflect across the vertical axis.
    pub flip_horizontal: bool,
    /// Reflect across the horizontal axis.
    pub flip_vertical: bool,
}

impl SideTransform {
    /// Add a quarter turn clockwise.
    pub fn rotate_clockwise(&mut self) {
        self.quarter_turns = (self.quarter_turns + 1) % 4;
    }

    /// Add a quarter turn counterclockwise.
    pub fn rotate_counterclockwise(&mut self) {
        self.quarter_turns = (self.quarter_turns + 3) % 4;
    }

    /// Toggle the reflection across the vertical axis.
    pub fn toggle_flip_horizontal(&mut self) {
        self.flip_horizontal = !self.flip_horizontal;
    }

    /// Toggle the reflection across the horizontal axis.
    pub fn toggle_flip_vertical(&mut self) {
        self.flip_vertical = !self.flip_vertical;
    }

    /// True when the transform leaves the image as it was decoded.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.quarter_turns.is_multiple_of(4) && !self.flip_horizontal && !self.flip_vertical
    }

    /// Apply the transform.
    ///
    /// # Errors
    /// Returns whatever allocating the turned or reflected buffer reports.
    pub fn apply(self, image: &RgbaImage) -> ca_image::Result<RgbaImage> {
        let mut current = match self.quarter_turns % 4 {
            1 => transform::rotate(image, Rotation::Clockwise90)?,
            2 => transform::rotate(image, Rotation::Half180)?,
            3 => transform::rotate(image, Rotation::Counterclockwise90)?,
            _ => image.clone(),
        };
        if self.flip_horizontal {
            current = transform::flip_horizontal(&current)?;
        }
        if self.flip_vertical {
            current = transform::flip_vertical(&current)?;
        }
        Ok(current)
    }
}

/// Everything one run needs beside the two files.
// The toggles are independent settings with independent stored names, so
// grouping them into enums would change what the settings document holds.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct Settings {
    /// How the difference pane renders.
    pub mode: DisplayMode,
    /// Greatest per-channel difference still treated as unimportant.
    pub tolerance: u8,
    /// Whether differences at or below the tolerance count and render as
    /// matches.
    pub ignore_unimportant: bool,
    /// Weight of the left image in blend mode, as a percentage.
    pub blend_percent: u8,
    /// Side shown by single side mode, and by the blend toggle.
    pub side: Side,
    /// Whether the alpha channel is left out of the comparison.
    pub ignore_alpha: bool,
    /// Whether two fully transparent pixels count as equal.
    pub transparent_pixels_equal: bool,
    /// Displacement applied to the right image.
    pub offset: Offset,
    /// Whether the smaller image is enlarged to the larger one's scale.
    pub auto_scale: bool,
    /// Transform applied to the left image.
    pub left_transform: SideTransform,
    /// Transform applied to the right image.
    pub right_transform: SideTransform,
    /// Color substitutions treated as unimportant.
    pub replacements: Vec<ca_image::compare::Replacement>,
    /// Tints used by tolerance mode.
    pub colors: ToleranceColors,
    /// Bounds applied to a decoded image.
    pub decode_limits: Limits,
    /// Bounds applied to a comparison result.
    pub result_limits: Limits,
    /// Format that reads the left file, or `None` to follow the file's bytes
    /// and name.
    pub left_format: Option<SourceFormat>,
    /// Format that reads the right file.
    pub right_format: Option<SourceFormat>,
}

impl Default for Settings {
    fn default() -> Self {
        let view = ca_image::settings::PictureViewSettings::default();
        Self {
            mode: DisplayMode::Tolerance,
            tolerance: view.tolerance,
            ignore_unimportant: view.ignore_unimportant,
            blend_percent: view.blend_percent,
            side: Side::Left,
            ignore_alpha: false,
            transparent_pixels_equal: ca_image::settings::provisional::TRANSPARENT_PIXELS_EQUAL,
            offset: Offset::zero(),
            auto_scale: view.auto_scale,
            left_transform: SideTransform::default(),
            right_transform: SideTransform::default(),
            replacements: Vec::new(),
            colors: view.colors,
            decode_limits: Limits::default(),
            result_limits: Limits::for_result(),
            left_format: None,
            right_format: None,
        }
    }
}

impl Settings {
    fn compare_options(&self, left: Fidelity, right: Fidelity) -> CompareOptions {
        CompareOptions {
            mode: self.mode,
            tolerance: self.tolerance,
            ignore_unimportant: self.ignore_unimportant,
            blend_percent: self.blend_percent.min(100),
            side: self.side,
            offset: self.offset,
            ignore_alpha: self.ignore_alpha,
            transparent_pixels_equal: self.transparent_pixels_equal,
            replacements: self.replacements.clone(),
            colors: self.colors.clone(),
            result_limits: self.result_limits,
            left_fidelity: left,
            right_fidelity: right,
        }
    }

    /// The transform of one side.
    #[must_use]
    pub fn transform(&self, side: Side) -> SideTransform {
        match side {
            Side::Left => self.left_transform,
            Side::Right => self.right_transform,
        }
    }

    /// The transform of one side, for changing.
    pub fn transform_mut(&mut self, side: Side) -> &mut SideTransform {
        match side {
            Side::Left => &mut self.left_transform,
            Side::Right => &mut self.right_transform,
        }
    }

    /// The format one side is read with.
    #[must_use]
    pub fn format(&self, side: Side) -> Option<&SourceFormat> {
        match side {
            Side::Left => self.left_format.as_ref(),
            Side::Right => self.right_format.as_ref(),
        }
    }
}

/// One decoded file and what the decoder reported about it.
#[derive(Debug, Clone)]
pub struct SideImage {
    /// The decoded pixels, as they came out of the file.
    pub image: Arc<RgbaImage>,
    /// What the decoder reported.
    pub metadata: Metadata,
}

impl SideImage {
    /// The size and stored depth, as the status bar shows them.
    #[must_use]
    pub fn dimensions_label(&self) -> String {
        format!(
            "{} x {} x {}",
            self.metadata.width, self.metadata.height, self.metadata.bits_per_pixel
        )
    }

    /// A sentence naming what the decode dropped, when it dropped anything.
    #[must_use]
    pub fn fidelity_note(&self) -> Option<String> {
        let fidelity = self.metadata.fidelity;
        if !fidelity.reduced() {
            return None;
        }
        let mut parts = Vec::new();
        if fidelity.precision_reduced {
            parts.push(format!(
                "stored at {} bits per channel and compared at 8",
                fidelity.bits_per_channel
            ));
        }
        if fidelity.cmyk {
            parts.push("stored as cyan, magenta, yellow and black".to_owned());
        }
        if fidelity.icc_profile {
            parts.push("carries a color profile that is not applied".to_owned());
        }
        Some(parts.join("; "))
    }
}

/// Both decoded files.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The left file.
    pub left: SideImage,
    /// The right file.
    pub right: SideImage,
}

/// A finished comparison, with everything the panes draw.
#[derive(Debug)]
pub struct Outcome {
    /// The left image as it is shown, after its transform and any scaling.
    pub left: Arc<RgbaImage>,
    /// The right image as it is shown.
    pub right: Arc<RgbaImage>,
    /// Reduced left image when the source is large enough to need one.
    pub left_preview: Option<Arc<RgbaImage>>,
    /// Reduced right image when the source is large enough to need one.
    pub right_preview: Option<Arc<RgbaImage>>,
    /// The rendered difference buffer.
    pub result: Arc<RgbaImage>,
    /// A reduced copy of the difference buffer, when the full one is large.
    pub preview: Option<Arc<RgbaImage>>,
    /// The class of every result pixel.
    pub mask: Arc<ClassMask>,
    /// The counters over the result.
    pub totals: Totals,
    /// Tolerance used to calculate the counters.
    pub tolerance: u8,
    /// Position of the result's top-left corner in the left image's
    /// coordinates.
    pub origin: Offset,
    /// Displacement the run compared at.
    pub offset: Offset,
}

/// What a run posts back.
#[derive(Debug)]
pub enum Message {
    /// A step of the pipeline has begun.
    Progress(&'static str),
    /// One side decoded.
    SideReady(Side, Box<SideImage>),
    /// One side could not be read.
    SideFailed(Side, String),
    /// The comparison is ready.
    Ready(Box<Outcome>),
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
}

impl Terminal for Message {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Message::Ready(_) | Message::Failed(_) | Message::Cancelled
        )
    }

    fn cancelled() -> Self {
        Message::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Message::Failed(detail)
    }
}

/// Read both files, decode them and compare them, on a worker thread.
#[must_use]
pub fn spawn_load(
    left: PathBuf,
    right: PathBuf,
    settings: Settings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<Message> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            emitter.send(Message::Progress("Reading"));
            let Some(sources) = decode_both(&left, &right, &settings, emitter, cancel) else {
                return;
            };
            run_compare(&sources, &settings, emitter, cancel);
        },
        notify,
    )
}

/// Compare two images already decoded, on a worker thread.
///
/// Nothing is read from disk, so a change to the tolerance or the offset does
/// not observe a file that changed under the view.
#[must_use]
pub fn spawn_compare(
    sources: Sources,
    settings: Settings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<Message> {
    Job::spawn_notifying(
        move |emitter, cancel| run_compare(&sources, &settings, emitter, cancel),
        notify,
    )
}

fn decode_both(
    left: &Path,
    right: &Path,
    settings: &Settings,
    emitter: &Emitter<Message>,
    cancel: &Cancel,
) -> Option<Sources> {
    let left_side = read_side(left, Side::Left, settings, emitter, cancel);
    if cancel.is_cancelled() {
        return None;
    }
    let right_side = read_side(right, Side::Right, settings, emitter, cancel);
    if let (Some(left), Some(right)) = (left_side, right_side) {
        return Some(Sources { left, right });
    }
    emitter.send(Message::Failed(
        "One side could not be read, so there is nothing to compare.".to_owned(),
    ));
    None
}

fn read_side(
    path: &Path,
    side: Side,
    settings: &Settings,
    emitter: &Emitter<Message>,
    cancel: &Cancel,
) -> Option<SideImage> {
    match decode_file(path, settings.format(side), settings, cancel) {
        Ok(decoded) => {
            let image = SideImage {
                image: Arc::new(decoded.image),
                metadata: decoded.metadata,
            };
            emitter.send(Message::SideReady(side, Box::new(image.clone())));
            Some(image)
        }
        Err(text) => {
            emitter.send(Message::SideFailed(side, text));
            None
        }
    }
}

/// Decode one file, turning every failure into a sentence a pane can show.
fn decode_file(
    path: &Path,
    format: Option<&SourceFormat>,
    settings: &Settings,
    cancel: &Cancel,
) -> Result<ca_image::decode::DecodedImage, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let reader = std::io::BufReader::new(file);
    let options = DecodeOptions {
        limits: settings.decode_limits,
        count_frames: true,
        ..DecodeOptions::default()
    };
    // A file with no signature still resolves through the format its masks
    // name, so the extension is the fallback the decoder is told about.
    let named = match format {
        Some(named) => Some(named.clone()),
        None => path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(SourceFormat::from_extension),
    };
    match ca_image::decode_reader_as_with_cancel(reader, named.as_ref(), &options, &Flag(cancel)) {
        Ok(decoded) => Ok(decoded),
        // A named format the bytes do not match is worth one more try by
        // signature, otherwise a mislabelled file reports the wrong reason.
        Err(error) if named.is_some() => {
            let file =
                std::fs::File::open(path).map_err(|open| format!("{}: {open}", path.display()))?;
            ca_image::decode_reader_as_with_cancel(
                std::io::BufReader::new(file),
                None,
                &options,
                &Flag(cancel),
            )
            .map_err(|_| describe(path, &error))
        }
        Err(error) => Err(describe(path, &error)),
    }
}

/// A failure as a pane states it: what file, what happened, and what to do
/// about it where there is something to do.
fn describe(path: &Path, error: &Error) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    match error {
        Error::Unsupported(detail) => {
            format!("{name}: this build has no decoder for this format. {detail}")
        }
        Error::InvalidData(detail) => format!("{name}: the image data is damaged. {detail}"),
        Error::Truncated => format!("{name}: the file ends before the image is complete."),
        Error::EmptyImage => format!("{name}: the image declares a zero width or height."),
        Error::DimensionsTooLarge {
            width,
            height,
            limit_width,
            limit_height,
        } => format!(
            "{name}: the image is {width} by {height} pixels, above the limit of \
             {limit_width} by {limit_height}."
        ),
        Error::TooLarge {
            needed,
            limit,
            unit,
        } => format!("{name}: decoding needs {needed} {unit}, above the limit of {limit}."),
        Error::LimitExceeded(detail) => {
            format!("{name}: the image crosses the decode limits. {detail}")
        }
        Error::Cancelled => format!("{name}: reading stopped."),
        other => format!("{name}: {other}"),
    }
}

fn run_compare(
    sources: &Sources,
    settings: &Settings,
    emitter: &Emitter<Message>,
    cancel: &Cancel,
) {
    emitter.send(Message::Progress("Aligning"));
    let left = match settings.left_transform.apply(&sources.left.image) {
        Ok(image) => image,
        Err(error) => {
            emitter.send(Message::Failed(format!("The left image: {error}")));
            return;
        }
    };
    let right = match settings.right_transform.apply(&sources.right.image) {
        Ok(image) => image,
        Err(error) => {
            emitter.send(Message::Failed(format!("The right image: {error}")));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    let (left, right) = if settings.auto_scale {
        match transform::auto_scale_within(&left, &right, Filter::Nearest, &settings.decode_limits)
        {
            Ok(pair) => pair,
            Err(error) => {
                emitter.send(Message::Failed(format!(
                    "The images cannot be brought to one scale: {error}"
                )));
                return;
            }
        }
    } else {
        (left, right)
    };
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(Message::Progress("Comparing"));
    let options = settings.compare_options(
        sources.left.metadata.fidelity,
        sources.right.metadata.fidelity,
    );
    let compared = match ca_image::compare(&left, &right, &options, &Flag(cancel)) {
        Ok(compared) => compared,
        Err(Error::Cancelled) => return,
        Err(error) => {
            emitter.send(Message::Failed(describe_compare(&error)));
            return;
        }
    };
    if cancel.is_cancelled() {
        return;
    }

    emitter.send(Message::Progress("Preparing the display"));
    let CompareResult {
        image,
        mask,
        totals,
        origin,
        ..
    } = compared;
    let left = Arc::new(left);
    let right = Arc::new(right);
    let left_preview = reduced_copy(&left).map(Arc::new);
    let right_preview = reduced_copy(&right).map(Arc::new);
    let preview = reduced_copy(&image).map(Arc::new);
    emitter.send(Message::Ready(Box::new(Outcome {
        left,
        right,
        left_preview,
        right_preview,
        result: Arc::new(image),
        preview,
        mask: Arc::new(mask),
        totals,
        tolerance: settings.tolerance,
        origin,
        offset: settings.offset,
    })));
}

fn describe_compare(error: &Error) -> String {
    match error {
        Error::ResultTooLarge { width, height, .. } => format!(
            "The comparison covers {width} by {height} pixels, which is above the limit. \
             Reduce the offset or turn off automatic scaling."
        ),
        other => other.to_string(),
    }
}

/// A reduced copy of `image`, or `None` when the image is already small
/// enough to be drawn whole.
fn reduced_copy(image: &RgbaImage) -> Option<RgbaImage> {
    let longest = image.width().max(image.height());
    if longest <= PREVIEW_MAX_SIDE {
        return None;
    }
    let scale = f64::from(PREVIEW_MAX_SIDE) / f64::from(longest);
    let side = |value: u32| -> u32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled = (f64::from(value) * scale).round() as u32;
        scaled.max(1)
    };
    transform::resize(
        image,
        side(image.width()),
        side(image.height()),
        Filter::Nearest,
    )
    .ok()
}

/// The worker's cancellation flag in the form the engine takes.
struct Flag<'a>(&'a Cancel);

impl ca_image::Cancel for Flag<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{reduced_copy, SideTransform, PREVIEW_MAX_SIDE};
    use ca_image::RgbaImage;

    #[test]
    fn four_quarter_turns_are_the_identity() {
        let mut transform = SideTransform::default();
        for _ in 0..4 {
            transform.rotate_clockwise();
        }
        assert!(transform.is_identity());
        transform.rotate_counterclockwise();
        assert_eq!(transform.quarter_turns, 3);
        transform.rotate_clockwise();
        assert!(transform.is_identity());
    }

    #[test]
    fn a_quarter_turn_swaps_the_dimensions() {
        let image = RgbaImage::filled(4, 2, [1, 2, 3, 4]).unwrap();
        let transform = SideTransform {
            quarter_turns: 1,
            ..SideTransform::default()
        };
        let turned = transform.apply(&image).unwrap();
        assert_eq!((turned.width(), turned.height()), (2, 4));
    }

    #[test]
    fn a_flip_is_its_own_inverse() {
        let mut image = RgbaImage::new(3, 3).unwrap();
        image.set_pixel(0, 0, [9, 9, 9, 255]);
        let transform = SideTransform {
            flip_horizontal: true,
            flip_vertical: true,
            ..SideTransform::default()
        };
        let once = transform.apply(&image).unwrap();
        let twice = transform.apply(&once).unwrap();
        assert_eq!(twice, image);
    }

    #[test]
    fn a_small_buffer_needs_no_reduced_copy() {
        let image = RgbaImage::filled(64, 64, [0, 0, 0, 255]).unwrap();
        assert!(reduced_copy(&image).is_none());
    }

    #[test]
    fn a_large_buffer_reduces_to_the_stated_side() {
        let image = RgbaImage::filled(PREVIEW_MAX_SIDE * 2 + 10, 100, [0, 0, 0, 255]).unwrap();
        let reduced = reduced_copy(&image).unwrap();
        assert_eq!(reduced.width(), PREVIEW_MAX_SIDE);
        assert!(reduced.height() >= 1);
    }
}
