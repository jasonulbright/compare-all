//! The one conversion from stored settings to the options the image engine
//! takes.

use crate::jobs::{Settings, SideTransform};
use ca_image::compare::{DisplayMode, Offset, Replacement, Side};
use ca_session::settings::binary::{
    PictureCompareSettings, PictureComparisonSettings, PictureDisplayMode, PictureSide,
};
use ca_session::settings::common::ReplacementItem;
use ca_session::settings::ReplacementSettings;

/// Greatest share of the second image a blended view can show.
const FULL_BLEND: u8 = 100;

/// Quarter turns in one full turn. A stored count is reduced by it, so an
/// arbitrary number still names one of the four orientations.
const QUARTER_TURNS_PER_TURN: u32 = 4;

/// The rendering a stored display mode names.
///
/// A mode this build has no renderer for falls back to the tolerance pass
/// rather than refusing the session.
#[must_use]
pub fn display_mode(mode: &PictureDisplayMode) -> DisplayMode {
    match mode {
        PictureDisplayMode::MismatchRange => DisplayMode::MismatchRange,
        PictureDisplayMode::Blend => DisplayMode::Blend,
        PictureDisplayMode::SingleSide => DisplayMode::SingleSide,
        PictureDisplayMode::ChannelDifference => DisplayMode::ChannelDifference,
        PictureDisplayMode::ChannelXor => DisplayMode::ChannelXor,
        _ => DisplayMode::Tolerance,
    }
}

/// The stored name of a rendering.
#[must_use]
pub fn display_mode_setting(mode: DisplayMode) -> PictureDisplayMode {
    match mode {
        DisplayMode::Tolerance => PictureDisplayMode::Tolerance,
        DisplayMode::MismatchRange => PictureDisplayMode::MismatchRange,
        DisplayMode::Blend => PictureDisplayMode::Blend,
        DisplayMode::SingleSide => PictureDisplayMode::SingleSide,
        DisplayMode::ChannelDifference => PictureDisplayMode::ChannelDifference,
        DisplayMode::ChannelXor => PictureDisplayMode::ChannelXor,
    }
}

/// The side a stored choice names. A side this build does not know reads as
/// the left one.
#[must_use]
pub fn side(stored: &PictureSide) -> Side {
    match stored {
        PictureSide::Right => Side::Right,
        _ => Side::Left,
    }
}

/// The stored name of a side.
#[must_use]
pub fn side_setting(side: Side) -> PictureSide {
    match side {
        Side::Left => PictureSide::Left,
        Side::Right => PictureSide::Right,
    }
}

/// The transform of one side a stored turn count and the two reflections state.
fn transform(quarter_turns: u32, flip_horizontal: bool, flip_vertical: bool) -> SideTransform {
    // The engine counts turns in a byte, so an arbitrary stored count is
    // reduced to the orientation it names before it is narrowed.
    let reduced = quarter_turns % QUARTER_TURNS_PER_TURN;
    SideTransform {
        quarter_turns: u8::try_from(reduced).unwrap_or(0),
        flip_horizontal,
        flip_vertical,
    }
}

/// The color a replacement entry names, or `None` when the text is not a
/// color this build reads.
///
/// The text is read as red, green, blue and an optional alpha in hexadecimal,
/// with or without a leading marker. Alpha defaults to fully opaque.
#[must_use]
pub fn color_of(text: &str) -> Option<[u8; 4]> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 && digits.len() != 8 {
        return None;
    }
    if !digits.chars().all(|value| value.is_ascii_hexdigit()) {
        return None;
    }
    let mut color = [0u8, 0, 0, 255];
    for (index, slot) in color.iter_mut().enumerate() {
        let Some(pair) = digits.get(index * 2..index * 2 + 2) else {
            break;
        };
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(color)
}

/// The text a stored entry carries for one color.
#[must_use]
pub fn color_text(color: [u8; 4]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        color[0], color[1], color[2], color[3]
    )
}

/// The color substitutions a stored replacement group states.
///
/// An entry whose two sides are not both colors this build reads is left out,
/// so a rule written for another comparison type cannot change what the pixels
/// are compared against.
#[must_use]
pub fn replacements_from(stored: &ReplacementSettings) -> Vec<Replacement> {
    stored
        .items
        .iter()
        .filter_map(|item| {
            Some(Replacement {
                matched: color_of(&item.find)?,
                replacement: color_of(&item.replace_with)?,
                unknown: std::collections::BTreeMap::new(),
            })
        })
        .collect()
}

/// The options a stored image session states, over the view's current ones.
///
/// The view holds display choices the settings do not cover, such as which
/// pane is shown, so the stored values are written over what the view has
/// rather than replacing the whole set.
#[must_use]
pub fn options_over(settings: &PictureCompareSettings, base: &Settings) -> Settings {
    let comparison = &settings.comparison;
    Settings {
        mode: display_mode(&comparison.display_mode),
        tolerance: comparison.tolerance,
        ignore_unimportant: comparison.ignore_unimportant,
        blend_percent: comparison.blend_percent.min(FULL_BLEND),
        side: side(&comparison.single_side),
        ignore_alpha: comparison.ignore_alpha,
        transparent_pixels_equal: comparison.transparent_pixels_equal,
        offset: Offset {
            x: comparison.offset_x,
            y: comparison.offset_y,
        },
        auto_scale: comparison.auto_scale,
        left_transform: transform(
            comparison.left_quarter_turns,
            comparison.left_flip_horizontal,
            comparison.left_flip_vertical,
        ),
        right_transform: transform(
            comparison.right_quarter_turns,
            comparison.right_flip_horizontal,
            comparison.right_flip_vertical,
        ),
        replacements: replacements_from(&settings.replacements),
        ..base.clone()
    }
}

/// The options a whole image comparison session states.
#[must_use]
pub fn options_of(settings: &PictureCompareSettings) -> Settings {
    options_over(settings, &Settings::default())
}

/// Writes the engine-facing values back into the settings they came from, so
/// a toolbar control and a settings page edit one value.
pub fn write_back(comparison: &mut PictureComparisonSettings, settings: &Settings) {
    comparison.display_mode = display_mode_setting(settings.mode);
    comparison.tolerance = settings.tolerance;
    comparison.ignore_unimportant = settings.ignore_unimportant;
    comparison.blend_percent = settings.blend_percent.min(FULL_BLEND);
    comparison.single_side = side_setting(settings.side);
    comparison.ignore_alpha = settings.ignore_alpha;
    comparison.transparent_pixels_equal = settings.transparent_pixels_equal;
    comparison.offset_x = settings.offset.x;
    comparison.offset_y = settings.offset.y;
    comparison.auto_scale = settings.auto_scale;
    comparison.left_quarter_turns = u32::from(settings.left_transform.quarter_turns);
    comparison.left_flip_horizontal = settings.left_transform.flip_horizontal;
    comparison.left_flip_vertical = settings.left_transform.flip_vertical;
    comparison.right_quarter_turns = u32::from(settings.right_transform.quarter_turns);
    comparison.right_flip_horizontal = settings.right_transform.flip_horizontal;
    comparison.right_flip_vertical = settings.right_transform.flip_vertical;
}

/// Writes the color substitutions back into the group they came from.
///
/// An entry the view could not read as a pair of colors was never handed to
/// the engine, so writing back replaces the list rather than merging into it.
pub fn write_replacements(stored: &mut ReplacementSettings, settings: &Settings) {
    let mut next = settings.replacements.iter();
    let mut items = Vec::with_capacity(stored.items.len().max(settings.replacements.len()));
    for item in &stored.items {
        if color_of(&item.find).is_some() && color_of(&item.replace_with).is_some() {
            if let Some(rule) = next.next() {
                let mut item = item.clone();
                item.find = color_text(rule.matched);
                item.replace_with = color_text(rule.replacement);
                items.push(item);
            }
        } else {
            // Keep entries this build cannot apply, including their flags and
            // fields added by a later build.
            items.push(item.clone());
        }
    }
    items.extend(next.map(|rule| ReplacementItem {
        find: color_text(rule.matched),
        replace_with: color_text(rule.replacement),
        ..ReplacementItem::default()
    }));
    stored.items = items;
}

/// The whole stored session the view's current options state.
#[must_use]
pub fn stored_from(settings: &Settings) -> PictureCompareSettings {
    stored_over(settings, &PictureCompareSettings::default())
}

/// Writes the engine-facing values over the last stored document, preserving
/// fields and replacement entries this build cannot interpret.
#[must_use]
pub fn stored_over(settings: &Settings, base: &PictureCompareSettings) -> PictureCompareSettings {
    let mut stored = base.clone();
    write_back(&mut stored.comparison, settings);
    write_replacements(&mut stored.replacements, settings);
    stored
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{color_of, color_text, display_mode, options_of, side, stored_from, stored_over};
    use crate::jobs::Settings;
    use ca_image::compare::{DisplayMode, Side};
    use ca_session::settings::binary::{PictureCompareSettings, PictureDisplayMode, PictureSide};
    use ca_session::settings::common::ReplacementItem;

    #[test]
    fn every_comparison_field_reaches_its_own_option() {
        let mut settings = PictureCompareSettings::default();
        settings.comparison.tolerance = 12;
        settings.comparison.blend_percent = 70;
        settings.comparison.offset_x = -4;
        settings.comparison.offset_y = 9;
        let options = options_of(&settings);
        assert_eq!(options.tolerance, 12);
        assert_eq!(options.blend_percent, 70);
        assert_eq!(options.offset.x, -4);
        assert_eq!(options.offset.y, 9);
    }

    #[test]
    fn every_toggle_reaches_its_own_option() {
        let mut settings = PictureCompareSettings::default();
        settings.comparison.ignore_unimportant = true;
        settings.comparison.ignore_alpha = true;
        settings.comparison.transparent_pixels_equal = false;
        settings.comparison.auto_scale = true;
        let options = options_of(&settings);
        assert!(options.ignore_unimportant);
        assert!(options.ignore_alpha);
        assert!(!options.transparent_pixels_equal);
        assert!(options.auto_scale);
    }

    #[test]
    fn every_display_mode_reaches_its_own_renderer() {
        for (stored, mode) in [
            (PictureDisplayMode::Tolerance, DisplayMode::Tolerance),
            (
                PictureDisplayMode::MismatchRange,
                DisplayMode::MismatchRange,
            ),
            (PictureDisplayMode::Blend, DisplayMode::Blend),
            (PictureDisplayMode::SingleSide, DisplayMode::SingleSide),
            (
                PictureDisplayMode::ChannelDifference,
                DisplayMode::ChannelDifference,
            ),
            (PictureDisplayMode::ChannelXor, DisplayMode::ChannelXor),
        ] {
            assert_eq!(display_mode(&stored), mode);
        }
        assert_eq!(
            display_mode(&PictureDisplayMode::Unknown(serde_json::json!("future"))),
            DisplayMode::Tolerance,
            "a mode this build has no renderer for still opens"
        );
        assert_eq!(side(&PictureSide::Right), Side::Right);
        assert_eq!(
            side(&PictureSide::Unknown(serde_json::json!("future"))),
            Side::Left
        );
    }

    #[test]
    fn a_turn_and_a_reflection_reach_the_side_they_name() {
        let mut settings = PictureCompareSettings::default();
        settings.comparison.left_quarter_turns = 7;
        settings.comparison.left_flip_vertical = true;
        settings.comparison.right_flip_horizontal = true;
        let options = options_of(&settings);
        assert_eq!(
            options.left_transform.quarter_turns, 3,
            "a count above one full turn names the orientation it reduces to"
        );
        assert!(options.left_transform.flip_vertical);
        assert!(options.right_transform.flip_horizontal);
        assert!(!options.right_transform.flip_vertical);
    }

    #[test]
    fn a_color_pair_reaches_the_engine_and_other_text_does_not() {
        let mut settings = PictureCompareSettings::default();
        settings.replacements.items = vec![
            ReplacementItem {
                find: "#FF0000".to_owned(),
                replace_with: "00FF00FF".to_owned(),
                ..ReplacementItem::default()
            },
            ReplacementItem {
                find: "the word".to_owned(),
                replace_with: "another word".to_owned(),
                ..ReplacementItem::default()
            },
        ];
        let rules = options_of(&settings).replacements;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].matched, [255, 0, 0, 255]);
        assert_eq!(rules[0].replacement, [0, 255, 0, 255]);
        assert_eq!(color_text([1, 2, 3, 4]), "#01020304");
        assert!(color_of("#12345").is_none());
        assert!(color_of("#GGGGGG").is_none());
    }

    #[test]
    fn stored_picture_settings_keep_unreadable_replacements_and_unknown_fields() {
        let mut base = PictureCompareSettings::default();
        base.comparison
            .unknown
            .insert("futureComparison".to_owned(), serde_json::json!({"x": 1}));
        base.replacements
            .unknown
            .insert("futureGroup".to_owned(), serde_json::json!(true));
        let mut readable = ReplacementItem {
            find: "#FF0000".to_owned(),
            replace_with: "#00FF00FF".to_owned(),
            match_case: true,
            whole_words_only: true,
            regular_expression: true,
            unknown: std::collections::BTreeMap::from([(
                "futureRule".to_owned(),
                serde_json::json!("kept"),
            )]),
            ..ReplacementItem::default()
        };
        let unreadable = ReplacementItem {
            find: "red".to_owned(),
            replace_with: "green".to_owned(),
            match_case: true,
            unknown: std::collections::BTreeMap::from([(
                "futureTextRule".to_owned(),
                serde_json::json!(7),
            )]),
            ..ReplacementItem::default()
        };
        base.replacements.items = vec![readable.clone(), unreadable.clone()];

        let mut engine = options_of(&base);
        engine.tolerance = 13;
        engine.replacements[0].matched = [1, 2, 3, 4];
        let written = stored_over(&engine, &base);
        readable.find = "#01020304".to_owned();
        assert_eq!(written.comparison.tolerance, 13);
        assert_eq!(
            written.comparison.unknown.get("futureComparison"),
            Some(&serde_json::json!({"x": 1}))
        );
        assert_eq!(
            written.replacements.unknown.get("futureGroup"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(written.replacements.items, vec![readable, unreadable]);
        assert!(written.replacements.items[0].match_case);
        assert!(written.replacements.items[0].whole_words_only);
        assert!(written.replacements.items[0].regular_expression);
    }

    #[test]
    fn a_blend_above_the_whole_image_is_held_at_it() {
        let mut settings = PictureCompareSettings::default();
        settings.comparison.blend_percent = 250;
        assert_eq!(options_of(&settings).blend_percent, 100);
    }

    #[test]
    fn the_display_choices_the_settings_do_not_cover_are_left_alone() {
        let mut base = Settings::default();
        base.decode_limits = ca_image::Limits::for_result();
        let limits = base.decode_limits;
        let options = super::options_over(&PictureCompareSettings::default(), &base);
        assert_eq!(options.decode_limits, limits);
    }

    #[test]
    fn every_engine_value_returns_to_the_settings_it_came_from() {
        let options = Settings {
            mode: DisplayMode::Blend,
            tolerance: 5,
            ignore_unimportant: true,
            blend_percent: 25,
            side: Side::Right,
            ignore_alpha: true,
            transparent_pixels_equal: false,
            auto_scale: true,
            offset: ca_image::compare::Offset { x: 3, y: -7 },
            left_transform: crate::jobs::SideTransform {
                quarter_turns: 2,
                flip_horizontal: true,
                flip_vertical: false,
            },
            right_transform: crate::jobs::SideTransform {
                quarter_turns: 1,
                flip_horizontal: false,
                flip_vertical: true,
            },
            replacements: vec![ca_image::compare::Replacement {
                matched: [10, 20, 30, 40],
                replacement: [40, 30, 20, 10],
                unknown: std::collections::BTreeMap::new(),
            }],
            ..Settings::default()
        };
        let back = options_of(&stored_from(&options));
        assert_eq!(back.mode, DisplayMode::Blend);
        assert_eq!(back.tolerance, 5);
        assert!(back.ignore_unimportant);
        assert_eq!(back.blend_percent, 25);
        assert_eq!(back.side, Side::Right);
        assert!(back.ignore_alpha);
        assert!(!back.transparent_pixels_equal);
        assert!(back.auto_scale);
        assert_eq!(back.offset.x, 3);
        assert_eq!(back.offset.y, -7);
        assert_eq!(back.left_transform, options.left_transform);
        assert_eq!(back.right_transform, options.right_transform);
        assert_eq!(back.replacements, options.replacements);
    }

    #[test]
    fn the_stored_defaults_produce_the_engine_defaults() {
        let options = options_of(&PictureCompareSettings::default());
        let engine = Settings::default();
        assert_eq!(options.tolerance, engine.tolerance);
        assert_eq!(options.blend_percent, engine.blend_percent);
        assert_eq!(options.mode, engine.mode);
        assert_eq!(options.side, engine.side);
        assert_eq!(options.auto_scale, engine.auto_scale);
        assert_eq!(options.ignore_alpha, engine.ignore_alpha);
        assert_eq!(
            options.transparent_pixels_equal,
            engine.transparent_pixels_equal
        );
        assert!(options.replacements.is_empty());
    }
}
