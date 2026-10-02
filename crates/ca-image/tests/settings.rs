//! Settings: defaults, and the forward compatibility round trip.

#![allow(clippy::unwrap_used)]

use ca_image::settings::{
    ConversionMethod, DisplayModeSetting, FilenameEncoding, FormatSelection, PictureFormatSettings,
    PictureSessionSettings, PictureViewSettings,
};
use ca_image::{CompareOptions, DisplayMode};
use serde_json::{json, Value};

fn round_trip<T>(value: &Value) -> Value
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let parsed: T = serde_json::from_value(value.clone()).unwrap();
    serde_json::to_value(&parsed).unwrap()
}

#[test]
fn unknown_session_fields_survive_a_round_trip() {
    let stored = json!({
        "specs": {
            "left": "a.png",
            "right": "b.png",
            "description": "two shots",
            "futureSpecField": [1, 2, 3]
        },
        "format": { "left": "detected", "right": "Portable Network Graphics" },
        "replacements": {
            "replacements": [
                { "matched": [255, 0, 0, 255], "replacement": [0, 0, 255, 255] }
            ]
        },
        "futureTab": { "enabled": true }
    });
    let written = round_trip::<PictureSessionSettings>(&stored);
    assert_eq!(written, stored);
}

#[test]
fn an_unknown_format_selection_keeps_its_stored_value() {
    let stored = json!({ "left": "detected", "right": { "profile": "camera" } });
    let parsed: ca_image::settings::FormatTab = serde_json::from_value(stored.clone()).unwrap();
    assert_eq!(parsed.left, FormatSelection::Detected);
    assert!(parsed.right.is_unknown());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), stored);
}

#[test]
fn unknown_format_settings_fields_survive_a_round_trip() {
    let stored = json!({
        "name": "Windows bitmap",
        "general": { "masks": ["*.bmp", "*.dib"], "description": "", "futureFlag": 7 },
        "conversion": {
            "method": "externalProgram",
            "loadingCommand": ["convert", "%s", "%t"],
            "filenameEncoding": "ansi"
        },
        "futureSection": null
    });
    let written = round_trip::<PictureFormatSettings>(&stored);
    assert_eq!(written, stored);

    let parsed: PictureFormatSettings = serde_json::from_value(stored).unwrap();
    assert_eq!(parsed.conversion.method, ConversionMethod::ExternalProgram);
    assert_eq!(parsed.conversion.filename_encoding, FilenameEncoding::Ansi);
    assert_eq!(parsed.general.masks.len(), 2);
}

#[test]
fn unknown_view_settings_survive_a_round_trip() {
    let stored = json!({
        "mode": "someFutureMode",
        "tolerance": 12,
        "blendPercent": 30,
        "futureToggle": true
    });
    let written = round_trip::<PictureViewSettings>(&stored);
    assert_eq!(written["mode"], json!("someFutureMode"));
    assert_eq!(written["futureToggle"], json!(true));
    assert_eq!(written["tolerance"], json!(12));

    let parsed: PictureViewSettings = serde_json::from_value(stored).unwrap();
    assert!(parsed.mode.is_unknown());
    assert!(DisplayMode::from_setting(&parsed.mode).is_none());
    assert!(CompareOptions::from_settings(&parsed, &[]).is_err());
}

#[test]
fn every_known_display_mode_maps_to_a_renderer() {
    let known = [
        DisplayModeSetting::Tolerance,
        DisplayModeSetting::MismatchRange,
        DisplayModeSetting::Blend,
        DisplayModeSetting::SingleSide,
        DisplayModeSetting::ChannelDifference,
        DisplayModeSetting::ChannelXor,
    ];
    for setting in known {
        assert!(
            DisplayMode::from_setting(&setting).is_some(),
            "{setting:?} has no renderer"
        );
        let tag = setting.tag().unwrap();
        assert_eq!(DisplayModeSetting::from_tag(tag), Some(setting));
    }
}

#[test]
fn stored_view_settings_build_comparison_options() {
    let view = PictureViewSettings {
        mode: DisplayModeSetting::Blend,
        tolerance: 9,
        blend_percent: 25,
        offset_x: -4,
        offset_y: 6,
        ..PictureViewSettings::default()
    };
    let options = CompareOptions::from_settings(&view, &[]).unwrap();
    assert_eq!(options.mode, DisplayMode::Blend);
    assert_eq!(options.tolerance, 9);
    assert_eq!(options.blend_percent, 25);
    assert_eq!(options.offset, ca_image::Offset::new(-4, 6));
}

#[test]
fn defaults_write_and_read_back_unchanged() {
    let defaults = PictureViewSettings::default();
    let text = serde_json::to_string(&defaults).unwrap();
    let parsed: PictureViewSettings = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, defaults);

    let session = PictureSessionSettings::default();
    let text = serde_json::to_string(&session).unwrap();
    let parsed: PictureSessionSettings = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, session);
}

/// Adds one distinctly named key to every object in the document, including
/// the objects nested inside arrays.
fn inject(value: &mut Value, path: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let key = format!("unknown_{path}");
            for (name, child) in map.iter_mut() {
                inject(child, &format!("{path}_{name}"), found);
            }
            map.insert(key.clone(), Value::String(path.to_owned()));
            found.push(key);
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                inject(item, &format!("{path}_{index}"), found);
            }
        }
        _ => {}
    }
}

fn assert_every_injected_key_survives<T>(mut document: Value)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let mut injected = Vec::new();
    inject(&mut document, "root", &mut injected);
    assert!(!injected.is_empty(), "no object carried an injected key");
    let written = round_trip::<T>(&document);
    assert_eq!(
        written, document,
        "the document changed across a round trip"
    );
    let text = serde_json::to_string(&written).unwrap();
    for key in injected {
        assert!(text.contains(&key), "{key} was dropped");
    }
}

#[test]
fn an_injected_key_at_every_level_survives_a_round_trip() {
    assert_every_injected_key_survives::<PictureSessionSettings>(json!({
        "specs": { "left": "a.png", "right": "b.png", "description": "two shots" },
        "format": { "left": "detected", "right": "detected" },
        "replacements": {
            "replacements": [
                { "matched": [255, 0, 0, 255], "replacement": [0, 0, 255, 255] },
                { "matched": [1, 2, 3, 4], "replacement": [5, 6, 7, 8] }
            ]
        }
    }));

    assert_every_injected_key_survives::<PictureFormatSettings>(json!({
        "name": "Windows bitmap",
        "general": { "masks": ["*.bmp"], "description": "" },
        "conversion": {
            "method": "externalProgram",
            "loadingCommand": ["convert", "%s", "%t"],
            "filenameEncoding": "ansi"
        }
    }));

    assert_every_injected_key_survives::<PictureViewSettings>(json!({
        "mode": "tolerance",
        "tolerance": 3,
        "ignoreUnimportant": false,
        "blendPercent": 50,
        "autoScale": false,
        "offsetX": 0,
        "offsetY": 0,
        "compareMetadata": true,
        "showTransparencyAsCheckerboarding": true,
        "colors": {
            "same": [255, 255, 255],
            "similar": [0, 0, 255],
            "different": [255, 0, 0]
        }
    }));
}

#[test]
fn an_empty_document_reads_as_the_defaults() {
    let parsed: PictureViewSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(parsed, PictureViewSettings::default());
}
