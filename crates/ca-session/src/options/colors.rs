//! Stored color choices.
//!
//! The document holds only the slots a person changed. A slot with no entry
//! resolves to the built-in table, so the measured values stay the defaults and
//! a build that adds a slot needs no migration. A slot name this build does not
//! know is kept and written back, because the map is data rather than a fixed
//! set of fields.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A color as the document stores it: `#RRGGBB`, upper case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
}

impl Rgb {
    /// A color from its three channels.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// The stored text of the color.
    #[must_use]
    pub fn to_text(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
    }

    /// Reads a color from `#RRGGBB`, or nothing when the text is not one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |start: usize| u8::from_str_radix(digits.get(start..start + 2)?, 16).ok();
        Some(Self {
            red: channel(0)?,
            green: channel(2)?,
            blue: channel(4)?,
        })
    }
}

/// The colors one variant of one view kind states for itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColorTable {
    /// Slot name to stored color text. A slot with no entry keeps the built-in
    /// value.
    pub slots: BTreeMap<String, String>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl ColorTable {
    /// The color stated for `slot`, when the entry parses.
    #[must_use]
    pub fn get(&self, slot: &str) -> Option<Rgb> {
        self.slots.get(slot).and_then(|text| Rgb::parse(text))
    }

    /// States a color for `slot`.
    pub fn set(&mut self, slot: &str, color: Rgb) {
        self.slots.insert(slot.to_owned(), color.to_text());
    }

    /// Drops whatever `slot` stated, so the built-in value applies again.
    pub fn clear(&mut self, slot: &str) {
        self.slots.remove(slot);
    }

    /// True when nothing is stated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// The two variants of one view kind's colors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColorPair {
    /// Colors used over a light background.
    pub light: ColorTable,
    /// Colors used over a dark background.
    pub dark: ColorTable,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl ColorPair {
    /// The table of one variant.
    #[must_use]
    pub fn table(&self, dark: bool) -> &ColorTable {
        if dark {
            &self.dark
        } else {
            &self.light
        }
    }

    /// The table of one variant, for editing.
    pub fn table_mut(&mut self, dark: bool) -> &mut ColorTable {
        if dark {
            &mut self.dark
        } else {
            &mut self.light
        }
    }

    /// True when neither variant states anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.light.is_empty() && self.dark.is_empty()
    }
}

/// One color group per view kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PaletteOptions {
    /// Text comparison and text merge input colors.
    pub text: ColorPair,
    /// Folder comparison colors.
    pub folder: ColorPair,
    /// Byte comparison colors.
    pub hex: ColorPair,
    /// Table comparison colors.
    pub table: ColorPair,
    /// Picture comparison colors.
    pub picture: ColorPair,
    /// Three way merge colors.
    pub merge: ColorPair,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// Which color group a page edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorGroup {
    /// Text comparison and text merge input colors.
    Text,
    /// Folder comparison colors.
    Folder,
    /// Byte comparison colors.
    Hex,
    /// Table comparison colors.
    Table,
    /// Picture comparison colors.
    Picture,
    /// Three way merge colors.
    Merge,
}

impl ColorGroup {
    /// Every group, in the order the pages list them.
    pub const ALL: &'static [ColorGroup] = &[
        ColorGroup::Text,
        ColorGroup::Folder,
        ColorGroup::Hex,
        ColorGroup::Table,
        ColorGroup::Picture,
        ColorGroup::Merge,
    ];

    /// The stored name of the group.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            ColorGroup::Text => "text",
            ColorGroup::Folder => "folder",
            ColorGroup::Hex => "hex",
            ColorGroup::Table => "table",
            ColorGroup::Picture => "picture",
            ColorGroup::Merge => "merge",
        }
    }
}

impl PaletteOptions {
    /// The pair of one group.
    #[must_use]
    pub const fn group(&self, group: ColorGroup) -> &ColorPair {
        match group {
            ColorGroup::Text => &self.text,
            ColorGroup::Folder => &self.folder,
            ColorGroup::Hex => &self.hex,
            ColorGroup::Table => &self.table,
            ColorGroup::Picture => &self.picture,
            ColorGroup::Merge => &self.merge,
        }
    }

    /// The pair of one group, for editing.
    pub fn group_mut(&mut self, group: ColorGroup) -> &mut ColorPair {
        match group {
            ColorGroup::Text => &mut self.text,
            ColorGroup::Folder => &mut self.folder,
            ColorGroup::Hex => &mut self.hex,
            ColorGroup::Table => &mut self.table,
            ColorGroup::Picture => &mut self.picture,
            ColorGroup::Merge => &mut self.merge,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{ColorGroup, ColorTable, PaletteOptions, Rgb};

    #[test]
    fn a_color_round_trips_through_its_text() {
        let color = Rgb::new(0x1C, 0x00, 0xFF);
        assert_eq!(color.to_text(), "#1C00FF");
        assert_eq!(Rgb::parse("#1c00ff"), Some(color));
    }

    #[test]
    fn text_that_is_not_a_color_reads_as_nothing() {
        for text in ["", "#12345", "#1234567", "1C00FF", "#GGGGGG"] {
            assert_eq!(Rgb::parse(text), None, "{text}");
        }
    }

    #[test]
    fn an_unset_slot_states_nothing_and_a_cleared_slot_states_nothing_again() {
        let mut table = ColorTable::default();
        assert!(table.is_empty());
        assert_eq!(table.get("same_line"), None);
        table.set("same_line", Rgb::new(1, 2, 3));
        assert_eq!(table.get("same_line"), Some(Rgb::new(1, 2, 3)));
        table.clear("same_line");
        assert!(table.is_empty());
    }

    #[test]
    fn every_group_reaches_its_own_pair() {
        let mut options = PaletteOptions::default();
        for group in ColorGroup::ALL {
            options
                .group_mut(*group)
                .table_mut(true)
                .set("same_line", Rgb::new(9, 9, 9));
        }
        for group in ColorGroup::ALL {
            assert!(!options.group(*group).is_empty(), "{}", group.id());
            assert!(options.group(*group).table(false).is_empty());
        }
    }
}
