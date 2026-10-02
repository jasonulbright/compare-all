//! Colors an HTML report paints with.
//!
//! The palette is an input, never a read of the application theme. A report is
//! printed and read on paper, so the built-in default is a light scheme with
//! dark text.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

/// An opaque color.
///
/// The type serializes as `#rrggbb`, so a stored palette holds no object a
/// later build could add a field to, and no value reaches the style sheet that
/// was not built from three bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
}

impl Color {
    /// Build a color from its three channels.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_hex(&text).ok_or_else(|| {
            serde::de::Error::custom("a color is six hexadecimal digits after a number sign")
        })
    }
}

fn parse_hex(text: &str) -> Option<Color> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |range: std::ops::Range<usize>| -> Option<u8> {
        u8::from_str_radix(digits.get(range)?, 16).ok()
    };
    Some(Color::new(channel(0..2)?, channel(2..4)?, channel(4..6)?))
}

/// Colors of one HTML report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReportPalette {
    /// Page background.
    pub background: Color,
    /// Body text.
    pub text: Color,
    /// Rule and table border.
    pub border: Color,
    /// Background of a heading row.
    pub header_background: Color,
    /// Text of a heading row.
    pub header_text: Color,
    /// Background of a row both sides share.
    pub same_background: Color,
    /// Background of a row that differs in a way that matters.
    pub difference_background: Color,
    /// Text of a row that differs in a way that matters.
    pub difference_text: Color,
    /// Background of a difference that does not matter.
    pub unimportant_background: Color,
    /// Text of a difference that does not matter.
    pub unimportant_text: Color,
    /// Background of a row present only on the left.
    pub left_orphan_background: Color,
    /// Background of a row present only on the right.
    pub right_orphan_background: Color,
    /// Background behind the differing part of a line.
    pub inline_background: Color,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl Default for ReportPalette {
    fn default() -> Self {
        Self {
            background: Color::new(0xff, 0xff, 0xff),
            text: Color::new(0x00, 0x00, 0x00),
            border: Color::new(0xa0, 0xa0, 0xa0),
            header_background: Color::new(0xe8, 0xec, 0xee),
            header_text: Color::new(0x00, 0x00, 0x00),
            same_background: Color::new(0xff, 0xff, 0xff),
            difference_background: Color::new(0xff, 0xd8, 0xd8),
            difference_text: Color::new(0x60, 0x00, 0x00),
            unimportant_background: Color::new(0xf0, 0xf0, 0xf0),
            unimportant_text: Color::new(0x50, 0x50, 0x50),
            left_orphan_background: Color::new(0xda, 0xea, 0xff),
            right_orphan_background: Color::new(0xda, 0xff, 0xda),
            inline_background: Color::new(0xff, 0xa8, 0xa8),
            unknown: BTreeMap::new(),
        }
    }
}

impl ReportPalette {
    /// A palette that paints nothing, for a monochrome document.
    ///
    /// Every background is the page background and every text color is the body
    /// text color, so a monochrome report separates its classes with weight,
    /// strikeout and a leading marker only.
    #[must_use]
    pub fn monochrome() -> Self {
        let paper = Color::new(0xff, 0xff, 0xff);
        let ink = Color::new(0x00, 0x00, 0x00);
        Self {
            background: paper,
            text: ink,
            border: Color::new(0x80, 0x80, 0x80),
            header_background: paper,
            header_text: ink,
            same_background: paper,
            difference_background: paper,
            difference_text: ink,
            unimportant_background: paper,
            unimportant_text: ink,
            left_orphan_background: paper,
            right_orphan_background: paper,
            inline_background: paper,
            unknown: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{Color, ReportPalette};

    #[test]
    fn a_color_prints_as_six_hexadecimal_digits() {
        assert_eq!(Color::new(0x0a, 0xff, 0x00).to_string(), "#0aff00");
    }

    #[test]
    fn a_color_survives_a_round_trip() {
        let color = Color::new(1, 2, 3);
        let text = serde_json::to_string(&color).expect("serialize");
        assert_eq!(text, "\"#010203\"");
        let back: Color = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, color);
    }

    #[test]
    fn a_malformed_color_is_rejected() {
        for text in ["\"010203\"", "\"#0102\"", "\"#gggggg\"", "\"red\""] {
            assert!(serde_json::from_str::<Color>(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_default_palette_is_light() {
        let palette = ReportPalette::default();
        assert_eq!(palette.background, Color::new(0xff, 0xff, 0xff));
        assert_eq!(palette.text, Color::new(0, 0, 0));
    }

    #[test]
    fn the_monochrome_palette_uses_one_background() {
        let palette = ReportPalette::monochrome();
        assert_eq!(palette.difference_background, palette.background);
        assert_eq!(palette.left_orphan_background, palette.background);
    }

    #[test]
    fn an_unknown_palette_field_survives_a_round_trip() {
        let text = r##"{"background":"#ffffff","futureTint":"#123456"}"##;
        let palette: ReportPalette = serde_json::from_str(text).expect("deserialize");
        let back = serde_json::to_string(&palette).expect("serialize");
        assert!(back.contains("futureTint"), "{back}");
    }
}
