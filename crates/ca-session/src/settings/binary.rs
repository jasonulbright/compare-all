//! Settings owned by the byte, image, registry, media and version kinds.

use super::common::{
    FormatOverride, FormatSettings, ReplacementOverride, ReplacementSettings, SpecsOverride,
    SpecsSettings,
};
use super::defaults::provisional;
use super::macros::{settings_composite, settings_group};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether two fully transparent pixels count as equal, whatever color
/// channels their transparency hides.
const TRANSPARENT_PIXELS_EQUAL: bool = true;

/// Quarter turns applied to a side before it is compared. Zero leaves the
/// image as it was decoded.
const NO_QUARTER_TURNS: u32 = 0;

/// How the character area of a byte pane reads a byte.
///
/// Only a single byte encoding keeps one character under each pair of digits,
/// so a multi byte encoding has no place here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum HexCharEncoding {
    /// Seven bit ASCII. Every byte above `0x7F` is unprintable.
    Ascii,
    /// The Windows single byte code page.
    #[default]
    Ansi,
    /// An encoding this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// How the difference pane of an image comparison renders.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PictureDisplayMode {
    /// Each pixel renders as a match, an unimportant difference or an
    /// important difference.
    #[default]
    Tolerance,
    /// Each pixel encodes the magnitude of its difference.
    MismatchRange,
    /// The two images combine by a percentage.
    Blend,
    /// One chosen side renders alone.
    SingleSide,
    /// Each channel renders as the absolute difference of the two sides.
    ChannelDifference,
    /// Each channel renders as the bitwise exclusive or of the two sides.
    ChannelXor,
    /// A mode this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// Which image a single-sided view renders.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PictureSide {
    /// The left image.
    #[default]
    Left,
    /// The right image.
    Right,
    /// A side this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// How bytes on the two sides are paired.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum HexAlignment {
    /// Search for the best correspondence, detecting insertions and deletions.
    #[default]
    Complete,
    /// Simpler and quicker pairing, intended for very large content.
    Fast,
    /// Pair byte N with byte N, detecting no insertions or deletions.
    None,
    /// A pairing rule this build does not understand, carried through
    /// unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

settings_group! {
    /// How bytes are paired and how content is opened.
    HexComparisonSettings / HexComparisonOverride {
        /// Pairing rule applied to the byte streams.
        alignment: HexAlignment = HexAlignment::Complete,
        /// Content at or below this size is read without holding a lock, so
        /// other programs can still write it.
        unlocked_load_limit_bytes: u64 = provisional::HEX_UNLOCKED_LOAD_LIMIT_BYTES,
        /// Bytes shown per row.
        bytes_per_row: u32 = provisional::HEX_BYTES_PER_ROW,
        /// How the character area reads a byte.
        char_encoding: HexCharEncoding = HexCharEncoding::Ansi,
    }
}

settings_group! {
    /// How pixels are compared and blended.
    // The toggles are independent settings with independent stored names, so
    // grouping them into enums would change what the document holds.
    #[allow(clippy::struct_excessive_bools)]
    PictureComparisonSettings / PictureComparisonOverride {
        /// How the difference pane renders.
        display_mode: PictureDisplayMode = PictureDisplayMode::Tolerance,
        /// Greatest per-pixel difference still treated as unimportant.
        tolerance: u8 = provisional::PICTURE_TOLERANCE,
        /// Differences at or below the tolerance count and render as matches.
        ignore_unimportant: bool = false,
        /// Percentage of the right image mixed into a blended view.
        blend_percent: u8 = provisional::PICTURE_BLEND_PERCENT,
        /// Image a single-sided view renders.
        single_side: PictureSide = PictureSide::Left,
        /// The alpha channel is left out of the comparison.
        ignore_alpha: bool = false,
        /// Two fully transparent pixels count as equal.
        transparent_pixels_equal: bool = TRANSPARENT_PIXELS_EQUAL,
        /// The smaller image is enlarged to the larger one's scale.
        auto_scale: bool = false,
        /// Horizontal offset applied to the right image, in pixels.
        offset_x: i32 = 0,
        /// Vertical offset applied to the right image, in pixels.
        offset_y: i32 = 0,
        /// Quarter turns clockwise applied to the left image.
        left_quarter_turns: u32 = NO_QUARTER_TURNS,
        /// The left image is reflected across its vertical axis.
        left_flip_horizontal: bool = false,
        /// The left image is reflected across its horizontal axis.
        left_flip_vertical: bool = false,
        /// Quarter turns clockwise applied to the right image.
        right_quarter_turns: u32 = NO_QUARTER_TURNS,
        /// The right image is reflected across its vertical axis.
        right_flip_horizontal: bool = false,
        /// The right image is reflected across its horizontal axis.
        right_flip_vertical: bool = false,
    }
}

settings_group! {
    /// Which media attributes count toward the comparison result.
    MediaImportanceSettings / MediaImportanceOverride {
        /// Differences in descriptive tags are important.
        tags_important: bool = true,
        /// Differences in the encoded stream are important.
        stream_important: bool = true,
        /// Differences in duration are important.
        duration_important: bool = true,
        /// Durations may differ by this many seconds before counting.
        duration_tolerance_seconds: u32 = provisional::MEDIA_DURATION_TOLERANCE_SECONDS,
        /// Tag names whose differences never count.
        unimportant_tags: Vec<String> = Vec::new(),
    }
}

settings_group! {
    /// Which version resource fields count toward the comparison result.
    VersionImportanceSettings / VersionImportanceOverride {
        /// Differences in the numeric file version are important.
        file_version_important: bool = true,
        /// Differences in the numeric product version are important.
        product_version_important: bool = true,
        /// Differences in descriptive string fields are important.
        string_fields_important: bool = true,
        /// String field names whose differences never count.
        unimportant_fields: Vec<String> = Vec::new(),
    }
}

settings_composite! {
    /// Settings a byte comparison session owns.
    HexCompareSettings / HexCompareOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Format reading each side.
        format: FormatSettings => FormatOverride,
        /// Byte pairing and load behavior.
        comparison: HexComparisonSettings => HexComparisonOverride,
    }
}

settings_composite! {
    /// Settings an image comparison session owns.
    PictureCompareSettings / PictureCompareOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Format reading each side.
        format: FormatSettings => FormatOverride,
        /// Pixel tolerance, blending and offset.
        comparison: PictureComparisonSettings => PictureComparisonOverride,
        /// Substitutions treated as unimportant in image metadata.
        replacements: ReplacementSettings => ReplacementOverride,
    }
}

settings_composite! {
    /// Settings a registry comparison session owns. The kind has no further
    /// setting groups.
    RegistryCompareSettings / RegistryCompareOverride {
        /// Hives, keys or registry files, and description.
        specs: SpecsSettings => SpecsOverride,
    }
}

settings_composite! {
    /// Settings a media comparison session owns.
    MediaCompareSettings / MediaCompareOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Which media attributes count.
        importance: MediaImportanceSettings => MediaImportanceOverride,
    }
}

settings_composite! {
    /// Settings a version resource comparison session owns.
    VersionCompareSettings / VersionCompareOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Which resource fields count.
        importance: VersionImportanceSettings => VersionImportanceOverride,
    }
}
