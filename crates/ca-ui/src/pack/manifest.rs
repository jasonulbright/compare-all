//! The manifest of a theme pack and the checks every manifest passes.
//!
//! A manifest is data from outside the program, so every field is checked
//! against a limit before anything is read on its behalf.

use super::{PackError, PackModes};
use crate::icons::Icon;
use ca_session::options::{ColorTable, PaletteOptions, Rgb};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The only manifest format this build reads.
pub const FORMAT: u32 = 1;
/// Name of the manifest file inside a pack folder.
pub const MANIFEST_FILE: &str = "pack.json";
/// Edge lengths, in pixels, a pack can supply.
pub const ICON_SIZES: [u32; 4] = [16, 24, 32, 48];
/// The path template used when a manifest names none.
pub const DEFAULT_PATTERN: &str = "icons/{size}/{name}.png";

/// Largest manifest accepted, in bytes.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Longest pack identifier, in characters.
pub const MAX_ID_CHARS: usize = 64;
/// Longest pack name, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// Longest version text, in characters.
pub const MAX_VERSION_CHARS: usize = 32;
/// Longest relative file path, in characters.
pub const MAX_PATH_CHARS: usize = 128;
/// Most explicit icon file entries one manifest lists.
pub const MAX_FILE_ENTRIES: usize = 600;
/// Most color slots one manifest states, over every group and mode.
pub const MAX_COLOR_SLOTS: usize = 2_048;
/// Longest color slot name, in characters.
pub const MAX_SLOT_CHARS: usize = 64;

/// One mode a pack can paint in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Light backgrounds.
    Light,
    /// Dark backgrounds.
    Dark,
}

/// Where the icon files of a pack are.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IconSection {
    /// Pixel sizes the pack supplies. Empty means the pack has no icons.
    pub sizes: Vec<u32>,
    /// Path of an icon file, with `{size}` and `{name}` in place of the pixel
    /// size and the icon name.
    pub pattern: Option<String>,
    /// Paths that replace the pattern, by icon name and then by pixel size.
    pub files: BTreeMap<String, BTreeMap<String, String>>,
}

/// A parsed and checked manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackManifest {
    /// Manifest format version.
    pub format: u32,
    /// Stable identifier: lower case letters, digits and hyphens.
    pub id: String,
    /// Name shown in the options.
    pub name: String,
    /// Version of the pack, free text.
    pub version: String,
    /// The modes the pack paints in.
    pub modes: Vec<Mode>,
    /// Colors that replace the built-in ones, in the shape of the stored
    /// options.
    #[serde(default)]
    pub colors: PaletteOptions,
    /// The icon files.
    #[serde(default)]
    pub icons: IconSection,
}

impl PackManifest {
    /// Parse and check manifest bytes.
    ///
    /// # Errors
    /// A message naming the first limit or rule the manifest breaks.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(PackError::new(format!(
                "The manifest is {} bytes. The limit is {MAX_MANIFEST_BYTES}.",
                bytes.len()
            )));
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| PackError::new(format!("The manifest is not valid: {error}.")))?;
        manifest.check()?;
        Ok(manifest)
    }

    /// The modes as flags.
    #[must_use]
    pub fn pack_modes(&self) -> PackModes {
        PackModes {
            light: self.modes.contains(&Mode::Light),
            dark: self.modes.contains(&Mode::Dark),
        }
    }

    /// The path of one icon file at one size, when the manifest names it.
    #[must_use]
    pub fn icon_path(&self, icon: Icon, size: u32) -> Option<String> {
        if !self.icons.sizes.contains(&size) {
            return None;
        }
        if let Some(path) = self
            .icons
            .files
            .get(icon.id())
            .and_then(|sizes| sizes.get(&size.to_string()))
        {
            return Some(path.clone());
        }
        let pattern = self.icons.pattern.as_deref().unwrap_or(DEFAULT_PATTERN);
        Some(
            pattern
                .replace("{size}", &size.to_string())
                .replace("{name}", icon.id()),
        )
    }

    fn check(&self) -> Result<(), PackError> {
        if self.format != FORMAT {
            return Err(PackError::new(if self.format > FORMAT {
                format!(
                    "The pack uses manifest format {}. This program reads format {FORMAT}.",
                    self.format
                )
            } else {
                format!("Manifest format {} is not supported.", self.format)
            }));
        }
        check_id(&self.id)?;
        check_text("name", &self.name, MAX_NAME_CHARS)?;
        check_text("version", &self.version, MAX_VERSION_CHARS)?;
        if self.modes.is_empty() {
            return Err(PackError::new("The pack names no mode.".to_owned()));
        }
        if self.modes.iter().collect::<BTreeSet<_>>().len() != self.modes.len() {
            return Err(PackError::new("The pack names a mode twice.".to_owned()));
        }
        self.check_icons()?;
        check_colors(&self.colors)
    }

    fn check_icons(&self) -> Result<(), PackError> {
        let section = &self.icons;
        for size in &section.sizes {
            if !ICON_SIZES.contains(size) {
                return Err(PackError::new(format!(
                    "Icon size {size} is not one of 16, 24, 32 or 48."
                )));
            }
        }
        if section.sizes.iter().collect::<BTreeSet<_>>().len() != section.sizes.len() {
            return Err(PackError::new(
                "The pack names an icon size twice.".to_owned(),
            ));
        }
        if let Some(pattern) = &section.pattern {
            if !pattern.contains("{size}") || !pattern.contains("{name}") {
                return Err(PackError::new(
                    "The icon path pattern needs both {size} and {name}.".to_owned(),
                ));
            }
            check_path(&pattern.replace("{size}", "16").replace("{name}", "icon"))?;
        }
        let mut entries = 0usize;
        for (name, sizes) in &section.files {
            for (size, path) in sizes {
                entries += 1;
                if entries > MAX_FILE_ENTRIES {
                    return Err(PackError::new(format!(
                        "The manifest lists more than {MAX_FILE_ENTRIES} icon files."
                    )));
                }
                let known = size
                    .parse::<u32>()
                    .is_ok_and(|value| section.sizes.contains(&value));
                if !known {
                    return Err(PackError::new(format!(
                        "The icon {name} names size {size}, which the pack does not list."
                    )));
                }
                check_path(path)?;
            }
        }
        Ok(())
    }
}

fn check_id(id: &str) -> Result<(), PackError> {
    let valid = !id.is_empty()
        && id.chars().count() <= MAX_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && id != "default";
    if valid {
        Ok(())
    } else {
        Err(PackError::new(format!(
            "The pack id must hold 1 to {MAX_ID_CHARS} lower case letters, digits or hyphens and must not be \"default\"."
        )))
    }
}

fn check_text(field: &str, value: &str, limit: usize) -> Result<(), PackError> {
    let chars = value.chars().count();
    if chars == 0 || chars > limit || value.chars().any(char::is_control) {
        return Err(PackError::new(format!(
            "The pack {field} must hold 1 to {limit} printable characters."
        )));
    }
    Ok(())
}

/// A path that stays inside the pack folder: forward slashes, plain names,
/// no drive, no parent step.
pub fn check_path(path: &str) -> Result<(), PackError> {
    let plain = |component: &str| {
        !component.is_empty()
            && component.chars().any(|c| c != '.')
            && component
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    if path.chars().count() > MAX_PATH_CHARS || !path.split('/').all(plain) {
        return Err(PackError::new(format!(
            "The file path \"{path}\" is not a plain relative path."
        )));
    }
    Ok(())
}

fn check_colors(colors: &PaletteOptions) -> Result<(), PackError> {
    let mut total = 0usize;
    for group in ca_session::options::ColorGroup::ALL {
        let pair = colors.group(*group);
        for table in [&pair.light, &pair.dark] {
            total += check_table(table)?;
        }
    }
    if total > MAX_COLOR_SLOTS {
        return Err(PackError::new(format!(
            "The pack states {total} colors. The limit is {MAX_COLOR_SLOTS}."
        )));
    }
    Ok(())
}

fn check_table(table: &ColorTable) -> Result<usize, PackError> {
    for (slot, text) in &table.slots {
        if slot.chars().count() > MAX_SLOT_CHARS || Rgb::parse(text).is_none() {
            return Err(PackError::new(format!(
                "The color for \"{slot}\" is not written as #RRGGBB."
            )));
        }
    }
    Ok(table.slots.len())
}
