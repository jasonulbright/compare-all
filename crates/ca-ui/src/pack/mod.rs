//! Theme packs: an icon set, a color set and the modes the pack paints in.
//!
//! A pack is a manifest plus optional PNG icon files. The same format serves
//! packs built into the program and packs installed in the per-user packs
//! folder; both go through [`load::load`] and the same limits. The built-in
//! look is the default pack and needs no files: any icon a pack does not supply
//! is painted from the built-in set.

pub mod host;
pub mod load;
pub mod manifest;

use ca_session::options::{ColorGroup, PaletteOptions};
use std::sync::Arc;

pub use host::{BuiltinPack, Catalog, PackEntry, PackHost, BUILTIN};
pub use load::{DirectoryFiles, LoadedPack, MemoryFiles, PackFiles};
pub use manifest::PackManifest;

/// Why a pack was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackError {
    message: String,
}

impl PackError {
    /// An error carrying `message`, which is shown to the person.
    #[must_use]
    pub const fn new(message: String) -> Self {
        Self { message }
    }

    /// The text to show.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for PackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PackError {}

/// Which modes a pack paints in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackModes {
    /// The pack has a light mode.
    pub light: bool,
    /// The pack has a dark mode.
    pub dark: bool,
}

impl PackModes {
    /// Both modes, as the built-in look has.
    pub const BOTH: Self = Self {
        light: true,
        dark: true,
    };

    /// Whether the pack lets the person choose between light and dark.
    #[must_use]
    pub const fn offers_choice(self) -> bool {
        self.light && self.dark
    }

    /// Whether to paint dark, given what the person's choice asks for.
    ///
    /// A pack with one mode paints that mode whatever the choice says.
    #[must_use]
    pub const fn dark_for(self, wants_dark: bool) -> bool {
        match (self.light, self.dark) {
            (true, false) => false,
            (false, true) => true,
            _ => wants_dark,
        }
    }

    /// Text for a list entry.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match (self.light, self.dark) {
            (true, true) => "light and dark",
            (true, false) => "light only",
            (false, true) => "dark only",
            (false, false) => "no mode",
        }
    }
}

/// The colors of a pack under the colors a person set: a slot the person set
/// wins over the pack, and a slot neither states keeps the built-in color.
#[must_use]
pub fn layered(pack: &PaletteOptions, user: &PaletteOptions) -> PaletteOptions {
    let mut result = user.clone();
    for group in ColorGroup::ALL {
        let from_pack = pack.group(*group);
        let target = result.group_mut(*group);
        for dark in [false, true] {
            let table = target.table_mut(dark);
            for (slot, text) in &from_pack.table(dark).slots {
                table
                    .slots
                    .entry(slot.clone())
                    .or_insert_with(|| text.clone());
            }
        }
    }
    result
}

fn active_key() -> egui::Id {
    egui::Id::new("theme-pack-active")
}

fn catalog_key() -> egui::Id {
    egui::Id::new("theme-pack-catalog")
}

/// Make `pack` the pack icons are painted from, or none for the built-in set.
pub fn install_active(ctx: &egui::Context, pack: Option<Arc<LoadedPack>>) {
    ctx.data_mut(|data| match pack {
        Some(pack) => data.insert_temp(active_key(), pack),
        None => data.remove::<Arc<LoadedPack>>(active_key()),
    });
}

/// The pack icons are painted from, when one is in force.
#[must_use]
pub fn active(ctx: &egui::Context) -> Option<Arc<LoadedPack>> {
    ctx.data(|data| data.get_temp::<Arc<LoadedPack>>(active_key()))
}

/// Publish the list of known packs for the options dialog to read.
pub fn install_catalog(ctx: &egui::Context, catalog: Arc<Catalog>) {
    ctx.data_mut(|data| data.insert_temp(catalog_key(), catalog));
}

/// The list of known packs, empty until the first scan finishes.
#[must_use]
pub fn catalog(ctx: &egui::Context) -> Arc<Catalog> {
    ctx.data(|data| data.get_temp::<Arc<Catalog>>(catalog_key()))
        .unwrap_or_default()
}
