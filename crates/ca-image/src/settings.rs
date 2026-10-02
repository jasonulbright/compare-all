//! Serde types for picture session settings and picture file format settings.
//!
//! Every struct keeps the fields it does not know in a flattened map, and every
//! enum keeps a tag it does not know in an `Unknown` arm that holds the raw
//! JSON. A document written by a newer build therefore survives a read and a
//! write through an older build unchanged.

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Default values of the picture comparison settings.
///
/// They are collected here so that a change edits one place. Nothing outside
/// this module hard codes them.
pub mod provisional {
    /// Greatest per-channel difference still treated as unimportant.
    pub const TOLERANCE: u8 = 0;

    /// Weight of the left image in blend mode, as a percentage.
    pub const BLEND_PERCENT: u8 = 50;

    /// Whether differences at or below the tolerance render as matches.
    pub const IGNORE_UNIMPORTANT: bool = false;

    /// Whether the smaller image is enlarged to the scale of the larger one.
    pub const AUTO_SCALE: bool = false;

    /// Whether image metadata takes part in the comparison.
    pub const COMPARE_METADATA: bool = true;

    /// Whether transparent regions render as a checkerboard.
    pub const SHOW_TRANSPARENCY_AS_CHECKERBOARD: bool = true;

    /// Edge length in pixels of one checkerboard square.
    pub const CHECKER_SIZE: u32 = 8;

    /// Lighter checkerboard square.
    pub const CHECKER_LIGHT: [u8; 4] = [255, 255, 255, 255];

    /// Darker checkerboard square.
    pub const CHECKER_DARK: [u8; 4] = [204, 204, 204, 255];

    /// Tint of matching pixels in tolerance mode. The documented result is
    /// shades of gray, which a neutral white tint produces.
    pub const COLOR_SAME: [u8; 3] = [255, 255, 255];

    /// Tint of unimportant differences in tolerance mode. The documented
    /// result is shades of blue.
    pub const COLOR_SIMILAR: [u8; 3] = [0, 0, 255];

    /// Tint of important differences in tolerance mode. The documented result
    /// is shades of red.
    pub const COLOR_DIFFERENT: [u8; 3] = [255, 0, 0];

    /// Pixels one arrow key press moves the difference offset.
    pub const OFFSET_NUDGE: i32 = 1;

    /// Pixels one modified arrow key press moves the difference offset.
    pub const OFFSET_NUDGE_LARGE: i32 = 10;

    /// Greatest accepted image width in pixels.
    pub const MAX_WIDTH: u32 = 65_535;

    /// Greatest accepted image height in pixels.
    pub const MAX_HEIGHT: u32 = 65_535;

    /// Greatest accepted pixel count of one image.
    pub const MAX_PIXELS: u64 = 268_435_456;

    /// Greatest accepted decoded size of one image in bytes.
    pub const MAX_DECODED_BYTES: u64 = 1 << 30;

    /// Greatest number of frames counted in an animated image before the count
    /// stops and reports saturation.
    pub const MAX_COUNTED_FRAMES: u32 = 4096;

    /// Greatest number of container bytes a frame count reads or skips before
    /// it stops. Counting never decodes pixels, so this bounds the whole cost
    /// of the count.
    pub const MAX_FRAME_SCAN_BYTES: u64 = 64 << 20;

    /// Greatest accepted width of a comparison result in pixels. Two images of
    /// the greatest accepted width, placed side by side, still fit.
    pub const MAX_RESULT_WIDTH: u32 = MAX_WIDTH * 2;

    /// Greatest accepted height of a comparison result in pixels.
    pub const MAX_RESULT_HEIGHT: u32 = MAX_HEIGHT * 2;

    /// Greatest accepted pixel count of a comparison result.
    pub const MAX_RESULT_PIXELS: u64 = MAX_PIXELS * 4;

    /// Greatest accepted size of a comparison result buffer in bytes.
    pub const MAX_RESULT_BYTES: u64 = MAX_DECODED_BYTES * 4;

    /// Whether two pixels that are fully transparent on both sides count as
    /// equal, whatever color channels they hide.
    pub const TRANSPARENT_PIXELS_EQUAL: bool = true;
}

/// Declares an enum with a string tag per variant plus an `Unknown` arm that
/// holds the raw JSON of a tag this build does not know.
macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $name:ident, default = $default:ident, {
            $( $(#[$vmeta:meta])* $variant:ident = $tag:literal ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        #[non_exhaustive]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )*
            /// A value written by another build, kept exactly as it was read.
            Unknown(Value),
        }

        impl $name {
            /// The stable tag of a known variant, or `None` for
            #[doc = concat!("[`", stringify!($name), "::Unknown`].")]
            #[must_use]
            pub fn tag(&self) -> Option<&'static str> {
                match self {
                    $( $name::$variant => Some($tag), )*
                    $name::Unknown(_) => None,
                }
            }

            /// The variant a tag names, or `None` when this build has none.
            #[must_use]
            pub fn from_tag(tag: &str) -> Option<Self> {
                match tag {
                    $( $tag => Some($name::$variant), )*
                    _ => None,
                }
            }

            /// True when this build has no variant for the stored value.
            #[must_use]
            pub fn is_unknown(&self) -> bool {
                matches!(self, $name::Unknown(_))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                $name::$default
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    $name::Unknown(value) => value.serialize(serializer),
                    other => match other.tag() {
                        Some(tag) => serializer.serialize_str(tag),
                        None => serializer.serialize_none(),
                    },
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = Value::deserialize(deserializer)?;
                if let Value::String(tag) = &value {
                    if let Some(known) = $name::from_tag(tag) {
                        return Ok(known);
                    }
                }
                Ok($name::Unknown(value))
            }
        }
    };
}

open_enum! {
    /// Which picture file format reads one side of a comparison.
    FormatSelection, default = Detected, {
        /// The format follows from the file masks of the configured formats.
        Detected = "detected",
    }
}

impl FormatSelection {
    /// A selection naming one configured picture format.
    #[must_use]
    pub fn named(name: &str) -> Self {
        FormatSelection::from_tag(name)
            .unwrap_or_else(|| FormatSelection::Unknown(Value::String(name.to_owned())))
    }

    /// The name of the picture format this selection names, if it names one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            FormatSelection::Unknown(Value::String(name)) => Some(name),
            _ => None,
        }
    }
}

open_enum! {
    /// How a picture file format loads a file.
    ConversionMethod, default = Internal, {
        /// The built-in decoders read the file directly.
        Internal = "internal",
        /// An external program converts the file before it is read.
        ExternalProgram = "externalProgram",
    }
}

open_enum! {
    /// Encoding of the file names handed to a conversion program.
    FilenameEncoding, default = Unicode, {
        /// File names pass as Unicode.
        Unicode = "unicode",
        /// File names pass in the system's ANSI code page.
        Ansi = "ansi",
    }
}

open_enum! {
    /// How the difference pane renders a comparison.
    DisplayModeSetting, default = Tolerance, {
        /// Each pixel renders as a match, an unimportant difference or an
        /// important difference.
        Tolerance = "tolerance",
        /// Each pixel encodes the magnitude of its difference.
        MismatchRange = "mismatchRange",
        /// The two images combine by a percentage.
        Blend = "blend",
        /// One chosen side renders alone.
        SingleSide = "singleSide",
        /// Each channel renders as the absolute difference of the two sides.
        ChannelDifference = "channelDifference",
        /// Each channel renders as the bitwise exclusive or of the two sides.
        ChannelXor = "channelXor",
    }
}

/// The Specs tab of the picture session settings: the two files and free text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpecsTab {
    /// Path of the image shown on the left.
    pub left: String,
    /// Path of the image shown on the right.
    pub right: String,
    /// Free text describing the comparison.
    pub description: String,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// The Format tab of the picture session settings: which picture format reads
/// each side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FormatTab {
    /// Format that reads the left file.
    pub left: FormatSelection,
    /// Format that reads the right file.
    pub right: FormatSelection,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// One color substitution treated as an unimportant difference.
///
/// The rule is directional: it matches when the left pixel equals `matched`
/// and the right pixel equals `replacement`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColorReplacement {
    /// Color matched on the left side, as red, green, blue and alpha.
    pub matched: [u8; 4],
    /// Color that replaces it on the right side.
    pub replacement: [u8; 4],
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// The Replacements tab of the picture session settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReplacementsTab {
    /// Substitutions treated as unimportant, applied in order.
    pub replacements: Vec<ColorReplacement>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// The picture session settings: exactly the Specs, Format and Replacements
/// tabs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PictureSessionSettings {
    /// The Specs tab.
    pub specs: SpecsTab,
    /// The Format tab.
    pub format: FormatTab,
    /// The Replacements tab.
    pub replacements: ReplacementsTab,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// The General tab of a picture file format.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FormatGeneralTab {
    /// File masks bound to this format.
    pub masks: Vec<String>,
    /// Free text. Built-in entries note limitations or requirements.
    pub description: String,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// The Conversion tab of a picture file format.
///
/// A picture format loads only, so there is no saving command. A conversion
/// program counts as successful only when it exits with code zero and leaves a
/// non-empty output file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FormatConversionTab {
    /// How the file is loaded.
    pub method: ConversionMethod,
    /// Command run to load the file, as a program followed by its arguments.
    /// The substitutions are `%s` for the source file, `%t` for the target
    /// file and `%o` for the original file.
    pub loading_command: Vec<String>,
    /// Encoding of the file names substituted into the command.
    pub filename_encoding: FilenameEncoding,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// A picture file format: exactly the General and Conversion tabs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PictureFormatSettings {
    /// Name of the format.
    pub name: String,
    /// The General tab.
    pub general: FormatGeneralTab,
    /// The Conversion tab.
    pub conversion: FormatConversionTab,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// Colors that tint the difference pane in tolerance mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ToleranceColorSettings {
    /// Tint of matching pixels.
    pub same: [u8; 3],
    /// Tint of differences at or below the tolerance.
    pub similar: [u8; 3],
    /// Tint of differences above the tolerance.
    pub different: [u8; 3],
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for ToleranceColorSettings {
    fn default() -> Self {
        Self {
            same: provisional::COLOR_SAME,
            similar: provisional::COLOR_SIMILAR,
            different: provisional::COLOR_DIFFERENT,
            unknown: BTreeMap::new(),
        }
    }
}

/// View state of one picture comparison: the toggles and values the view
/// commands change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
// The toggles are independent view commands with independent stored names, so
// grouping them into enums would change the stored shape.
#[allow(clippy::struct_excessive_bools)]
pub struct PictureViewSettings {
    /// How the difference pane renders.
    pub mode: DisplayModeSetting,
    /// Greatest per-channel difference still treated as unimportant.
    pub tolerance: u8,
    /// Whether differences at or below the tolerance render as matches.
    pub ignore_unimportant: bool,
    /// Weight of the left image in blend mode, as a percentage.
    pub blend_percent: u8,
    /// Whether the smaller image is enlarged to the scale of the larger one.
    pub auto_scale: bool,
    /// Horizontal offset in pixels applied to the right image.
    pub offset_x: i32,
    /// Vertical offset in pixels applied to the right image.
    pub offset_y: i32,
    /// Whether image metadata takes part in the comparison.
    pub compare_metadata: bool,
    /// Whether transparent regions render as a checkerboard.
    pub show_transparency_as_checkerboarding: bool,
    /// Tints used in tolerance mode.
    pub colors: ToleranceColorSettings,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

impl Default for PictureViewSettings {
    fn default() -> Self {
        Self {
            mode: DisplayModeSetting::Tolerance,
            tolerance: provisional::TOLERANCE,
            ignore_unimportant: provisional::IGNORE_UNIMPORTANT,
            blend_percent: provisional::BLEND_PERCENT,
            auto_scale: provisional::AUTO_SCALE,
            offset_x: 0,
            offset_y: 0,
            compare_metadata: provisional::COMPARE_METADATA,
            show_transparency_as_checkerboarding: provisional::SHOW_TRANSPARENCY_AS_CHECKERBOARD,
            colors: ToleranceColorSettings::default(),
            unknown: BTreeMap::new(),
        }
    }
}

/// File masks the picture session type is associated with by default.
pub const DEFAULT_MASKS: &[&str] = &["*.gif", "*.ico", "*.jpg", "*.png", "*.tif", "*.bmp"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_tag_survives_a_round_trip() {
        let Ok(value) = serde_json::from_str::<ConversionMethod>("\"somethingElse\"") else {
            return;
        };
        assert!(value.is_unknown());
        assert_eq!(value.tag(), None);
        let Ok(text) = serde_json::to_string(&value) else {
            return;
        };
        assert_eq!(text, "\"somethingElse\"");
    }

    #[test]
    fn a_known_tag_reads_back_as_its_variant() {
        let parsed = serde_json::from_str::<ConversionMethod>("\"externalProgram\"");
        assert_eq!(parsed.ok(), Some(ConversionMethod::ExternalProgram));
    }

    #[test]
    fn a_named_format_selection_keeps_its_name() {
        let selection = FormatSelection::named("Portable Network Graphics");
        assert_eq!(selection.name(), Some("Portable Network Graphics"));
        assert_eq!(
            FormatSelection::named("detected"),
            FormatSelection::Detected
        );
    }

    #[test]
    fn defaults_match_the_provisional_constants() {
        let view = PictureViewSettings::default();
        assert_eq!(view.tolerance, provisional::TOLERANCE);
        assert_eq!(view.blend_percent, provisional::BLEND_PERCENT);
        assert_eq!(view.colors.same, provisional::COLOR_SAME);
    }
}
