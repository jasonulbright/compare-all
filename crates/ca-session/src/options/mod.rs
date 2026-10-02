//! The application options document.
//!
//! One file in the settings directory holds every setting that belongs to the
//! program rather than to one comparison. It follows the stored-format rules:
//! it carries its own schema version, every object keeps the fields this build
//! does not know, every stored enum has an arm for a choice it does not know,
//! and a save that meets a document replaced by another instance combines the
//! two rather than dropping either.

pub mod colors;
pub mod commands;
pub mod openwith;
pub mod pages;
pub mod provisional;
pub mod reports;

use crate::error::{Error, Result};
use crate::store::{atomic_write, DocumentStamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use colors::{ColorGroup, ColorPair, ColorTable, PaletteOptions, Rgb};
pub use commands::{CommandOptions, ViewCommandOptions};
pub use openwith::{
    LaunchCommand, LaunchContext, LaunchSide, OpenWithEntry, OpenWithOptions, WorkingFolder,
};
pub use reports::{ReportOptions, ReportPreference, ReportTarget};

pub use pages::{
    AppearanceOptions, ArchiveOptions, BackupLocation, BackupOptions, ComparisonPriority,
    FileOperationOptions, FileViewOptions, FolderViewOptions, FontOptions, LineEndingsOnSave,
    NewSessionPlacement, NextDifferenceOptions, PictureOptions, QuickCompareMethod, StartupOptions,
    SynchronizeConfirmations, TabOptions, TextEditingOptions, ThemeChoice, ThumbnailMode,
    TweakOptions,
};

/// Highest options schema version this build writes and reads.
pub const OPTIONS_SCHEMA_VERSION: u32 = 1;

/// Name of the options document in the settings directory.
pub const OPTIONS_FILE: &str = "options.json";

/// Every setting that belongs to the program rather than to one comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProgramOptions {
    /// The version this document was written against.
    pub schema_version: u32,
    /// What happens at program start.
    pub startup: StartupOptions,
    /// Where a session opens.
    pub tabs: TabOptions,
    /// Theme, colors and fonts.
    pub appearance: AppearanceOptions,
    /// How folder listings are drawn.
    pub folder_views: FolderViewOptions,
    /// How file listings are drawn.
    pub file_views: FileViewOptions,
    /// How picture comparisons are drawn.
    pub picture: PictureOptions,
    /// How the shared text editor behaves.
    pub text_editing: TextEditingOptions,
    /// How the difference navigation commands behave.
    pub next_difference: NextDifferenceOptions,
    /// Backup copies taken before a file is replaced.
    pub backups: BackupOptions,
    /// What a file operation asks before it runs.
    pub file_operations: FileOperationOptions,
    /// Filename masks naming the archive formats.
    pub archives: ArchiveOptions,
    /// External programs the Open With menu offers.
    pub open_with: OpenWithOptions,
    /// Keyboard, menu and toolbar customization.
    pub commands: CommandOptions,
    /// The report settings each view kind was last run with.
    pub reports: ReportOptions,
    /// Low level settings with no page of their own.
    pub tweaks: TweakOptions,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
    /// The document this options set was read from, so a save can tell that
    /// something else replaced it.
    #[serde(skip)]
    stamp: DocumentStamp,
}

impl Default for ProgramOptions {
    fn default() -> Self {
        Self {
            schema_version: OPTIONS_SCHEMA_VERSION,
            startup: StartupOptions::default(),
            tabs: TabOptions::default(),
            appearance: AppearanceOptions::default(),
            folder_views: FolderViewOptions::default(),
            file_views: FileViewOptions::default(),
            picture: PictureOptions::default(),
            text_editing: TextEditingOptions::default(),
            next_difference: NextDifferenceOptions::default(),
            backups: BackupOptions::default(),
            file_operations: FileOperationOptions::default(),
            archives: ArchiveOptions::default(),
            open_with: OpenWithOptions::default(),
            commands: CommandOptions::default(),
            reports: ReportOptions::default(),
            tweaks: TweakOptions::default(),
            unknown: BTreeMap::new(),
            stamp: DocumentStamp::default(),
        }
    }
}

/// What a load of the options document found.
#[derive(Debug)]
pub struct OptionsLoad {
    /// The options, the built-in set when no document existed.
    pub options: Box<ProgramOptions>,
    /// Where a document that could not be read was moved to.
    pub recovered_backup: Option<PathBuf>,
    /// The version a newer build wrote, when it is higher than this one's.
    pub newer_schema: Option<u32>,
}

/// One category of the restore wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum RestoreCategory {
    /// Most of the options pages.
    ProgramOptions,
    /// The theme, the colors and the fonts.
    ThemeColorsFonts,
    /// The toolbars, the shortcuts and the menus.
    ToolbarsShortcutsMenus,
    /// Window positions and recently used lists.
    ProgramState,
    /// Named sessions.
    Sessions,
    /// Customized file formats.
    FileFormats,
    /// Named profiles.
    Profiles,
}

impl RestoreCategory {
    /// Every category, in the order the wizard lists them.
    pub const ALL: &'static [RestoreCategory] = &[
        RestoreCategory::ProgramOptions,
        RestoreCategory::ThemeColorsFonts,
        RestoreCategory::ToolbarsShortcutsMenus,
        RestoreCategory::ProgramState,
        RestoreCategory::Sessions,
        RestoreCategory::FileFormats,
        RestoreCategory::Profiles,
    ];

    /// The label the wizard shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            RestoreCategory::ProgramOptions => "Program options",
            RestoreCategory::ThemeColorsFonts => "Theme, colors and fonts",
            RestoreCategory::ToolbarsShortcutsMenus => "Toolbars, shortcuts and menus",
            RestoreCategory::ProgramState => "Program state",
            RestoreCategory::Sessions => "Sessions",
            RestoreCategory::FileFormats => "File formats",
            RestoreCategory::Profiles => "Profiles",
        }
    }

    /// True when this build carries out the category.
    ///
    /// A category the build has no store for is offered and stated as not
    /// carried out, rather than silently doing nothing.
    #[must_use]
    pub const fn is_carried_out(self) -> bool {
        matches!(
            self,
            RestoreCategory::ProgramOptions
                | RestoreCategory::ThemeColorsFonts
                | RestoreCategory::ToolbarsShortcutsMenus
                | RestoreCategory::Sessions
        )
    }
}

/// What the restore wizard was asked to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreSelection {
    /// The categories to restore.
    pub categories: Vec<RestoreCategory>,
    /// Remove every stored session rather than resetting them.
    pub delete_all_sessions: bool,
    /// Remove every customized file format rather than resetting it.
    pub delete_all_file_formats: bool,
    /// Remove every named profile rather than resetting it.
    pub delete_all_profiles: bool,
}

impl RestoreSelection {
    /// True when the named category was selected.
    #[must_use]
    pub fn holds(&self, category: RestoreCategory) -> bool {
        self.categories.contains(&category)
    }

    /// True when the wizard needs its second page.
    #[must_use]
    pub fn needs_second_page(&self) -> bool {
        self.holds(RestoreCategory::Sessions)
            || self.holds(RestoreCategory::FileFormats)
            || self.holds(RestoreCategory::Profiles)
    }
}

impl ProgramOptions {
    /// Takes every page of `edited`, keeping the record of which document this
    /// set was read from.
    ///
    /// An edited copy carries no stamp of its own, so assigning it whole would
    /// make the next save believe another writer had replaced the file.
    pub fn adopt(&mut self, edited: ProgramOptions) {
        let stamp = self.stamp.clone();
        *self = edited;
        self.stamp = stamp;
    }

    /// True when the document was written by a newer build.
    #[must_use]
    pub const fn is_newer_schema(&self) -> bool {
        self.schema_version > OPTIONS_SCHEMA_VERSION
    }

    /// Puts the categories named by `selection` back to their built-in values.
    ///
    /// Categories this build has no store for are left alone; the caller states
    /// them through [`RestoreCategory::is_carried_out`].
    pub fn restore(&mut self, selection: &RestoreSelection) {
        let built_in = ProgramOptions::default();
        if selection.holds(RestoreCategory::ProgramOptions) {
            let appearance = self.appearance.clone();
            let commands = self.commands.clone();
            let schema_version = self.schema_version;
            let unknown = std::mem::take(&mut self.unknown);
            let stamp = self.stamp.clone();
            *self = ProgramOptions {
                schema_version,
                appearance,
                commands,
                unknown,
                stamp,
                ..built_in.clone()
            };
        }
        if selection.holds(RestoreCategory::ThemeColorsFonts) {
            let unknown = std::mem::take(&mut self.appearance.unknown);
            self.appearance = AppearanceOptions {
                unknown,
                ..built_in.appearance.clone()
            };
        }
        if selection.holds(RestoreCategory::ToolbarsShortcutsMenus) {
            let unknown = std::mem::take(&mut self.commands.unknown);
            self.commands = CommandOptions {
                unknown,
                ..built_in.commands
            };
        }
    }

    /// Reads the document at `path`.
    ///
    /// A missing document yields the built-in options. A document that cannot
    /// be parsed is moved aside rather than overwritten, so nothing a person
    /// stated is lost to a later save.
    ///
    /// # Errors
    /// Returns [`Error::Io`] when the document exists but cannot be read or
    /// moved aside.
    pub fn load(path: &Path) -> Result<OptionsLoad> {
        let stamp = DocumentStamp::of(path)?;
        let bytes = match crate::document::read(path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(OptionsLoad {
                    options: Box::new(ProgramOptions {
                        stamp,
                        ..ProgramOptions::default()
                    }),
                    recovered_backup: None,
                    newer_schema: None,
                });
            }
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };

        let quarantined = |path: &Path| -> Result<OptionsLoad> {
            let backup = crate::store::quarantine(path)?;
            Ok(OptionsLoad {
                options: Box::new(ProgramOptions::default()),
                recovered_backup: Some(backup),
                newer_schema: None,
            })
        };

        let Ok(text) = String::from_utf8(bytes) else {
            return quarantined(path);
        };
        let Some((mut options, found)) = parse_document(&text) else {
            return quarantined(path);
        };
        let newer_schema = (found > OPTIONS_SCHEMA_VERSION).then_some(found);
        options.stamp = stamp;
        Ok(OptionsLoad {
            options: Box::new(options),
            recovered_backup: None,
            newer_schema,
        })
    }

    /// Reads the document at `path` and changes nothing on disk.
    ///
    /// A document that cannot be read or parsed yields `None` and stays where
    /// it is, so the full [`ProgramOptions::load`] that follows still reports
    /// it and moves it aside.
    #[must_use]
    pub fn peek(path: &Path) -> Option<Self> {
        let text = String::from_utf8(crate::document::read(path).ok()?).ok()?;
        parse_document(&text).map(|(options, _)| options)
    }

    /// Writes the document, replacing the one it was read from.
    ///
    /// # Errors
    /// Returns [`Error::DocumentChanged`] when the file on disk is no longer
    /// the one this set was read from, and [`Error::Io`] or [`Error::Parse`]
    /// when it cannot be written.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        if !DocumentStamp::of(path)?.is_same_document(&self.stamp) {
            return Err(Error::DocumentChanged {
                path: path.to_path_buf(),
            });
        }
        self.save_replacing(path)
    }

    /// Writes the document whatever is on disk.
    ///
    /// # Errors
    /// Returns [`Error::Io`] or [`Error::Parse`].
    pub fn save_replacing(&mut self, path: &Path) -> Result<()> {
        atomic_write(path, |buffer| {
            serde_json::to_writer_pretty(crate::document::Writer::new(buffer), self).map_err(
                |source| Error::Parse {
                    path: path.to_path_buf(),
                    source,
                },
            )
        })?;
        self.stamp = DocumentStamp::of(path)?;
        Ok(())
    }

    /// Writes the document, combining it with another writer's rather than
    /// refusing or overwriting.
    ///
    /// This writer's pages win, because it is the later of the two. Everything
    /// the other writer stored that this build has no field for is carried
    /// across, so a newer build's settings survive a save from this one.
    ///
    /// # Errors
    /// Returns [`Error::Io`] or [`Error::Parse`].
    pub fn save_merging(&mut self, path: &Path) -> Result<bool> {
        match self.save(path) {
            Ok(()) => Ok(false),
            Err(Error::DocumentChanged { .. }) => {
                let disk = ProgramOptions::load(path)?;
                self.take_unknown_from(&disk.options);
                self.schema_version = self.schema_version.max(disk.options.schema_version);
                self.stamp = disk.options.stamp.clone();
                self.save(path)?;
                Ok(true)
            }
            Err(other) => Err(other),
        }
    }

    /// Keeps every field of `other` this build has no place for.
    fn take_unknown_from(&mut self, other: &ProgramOptions) {
        fn merge(into: &mut BTreeMap<String, Value>, from: &BTreeMap<String, Value>) {
            for (key, value) in from {
                into.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        merge(&mut self.unknown, &other.unknown);
        merge(&mut self.startup.unknown, &other.startup.unknown);
        merge(&mut self.tabs.unknown, &other.tabs.unknown);
        merge(&mut self.appearance.unknown, &other.appearance.unknown);
        merge(&mut self.folder_views.unknown, &other.folder_views.unknown);
        merge(&mut self.file_views.unknown, &other.file_views.unknown);
        merge(&mut self.picture.unknown, &other.picture.unknown);
        merge(&mut self.text_editing.unknown, &other.text_editing.unknown);
        merge(
            &mut self.next_difference.unknown,
            &other.next_difference.unknown,
        );
        merge(&mut self.backups.unknown, &other.backups.unknown);
        merge(
            &mut self.file_operations.unknown,
            &other.file_operations.unknown,
        );
        merge(&mut self.archives.unknown, &other.archives.unknown);
        merge(&mut self.open_with.unknown, &other.open_with.unknown);
        merge(&mut self.commands.unknown, &other.commands.unknown);
        merge(&mut self.tweaks.unknown, &other.tweaks.unknown);
    }
}

/// The options a document states, with the schema version it carried.
fn parse_document(text: &str) -> Option<(ProgramOptions, u32)> {
    let mut document = serde_json::from_str::<Value>(text).ok()?;
    let found = document
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(0);
    if migrate(&mut document, found) {
        let migrated = serde_json::to_string(&document).ok()?;
        let mut options = serde_json::from_str::<ProgramOptions>(&migrated).ok()?;
        options.schema_version = found.max(OPTIONS_SCHEMA_VERSION);
        Some((options, found))
    } else {
        let mut options = serde_json::from_str::<ProgramOptions>(text).ok()?;
        options.schema_version = found.max(OPTIONS_SCHEMA_VERSION);
        Some((options, found))
    }
}

/// Brings a document forward from an older schema version.
///
/// Version 0 is a document written before the version field existed. It is
/// shaped like version 1 apart from carrying no version, so the step stamps one
/// on and leaves every other field alone.
fn migrate(document: &mut Value, found: u32) -> bool {
    if found >= OPTIONS_SCHEMA_VERSION {
        return false;
    }
    let Some(object) = document.as_object_mut() else {
        return false;
    };
    object.insert(
        "schemaVersion".to_owned(),
        Value::from(u64::from(OPTIONS_SCHEMA_VERSION)),
    );
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{
        ProgramOptions, RestoreCategory, RestoreSelection, Rgb, OPTIONS_FILE,
        OPTIONS_SCHEMA_VERSION,
    };
    use crate::options::ColorGroup;

    fn path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join(OPTIONS_FILE)
    }

    #[test]
    fn a_missing_document_yields_the_built_in_options() {
        let dir = tempfile::tempdir().unwrap();
        let load = ProgramOptions::load(&path(&dir)).unwrap();
        assert_eq!(*load.options, ProgramOptions::default());
        assert!(load.recovered_backup.is_none());
    }

    #[test]
    fn an_edited_document_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(&dir);
        let mut options = ProgramOptions::default();
        options.text_editing.tab_stop = 4;
        options.file_operations.confirm_delete = false;
        options
            .appearance
            .palettes
            .group_mut(ColorGroup::Text)
            .table_mut(true)
            .set("same_line", Rgb::new(0x10, 0x20, 0x30));
        options.save_replacing(&file).unwrap();

        let read = ProgramOptions::load(&file).unwrap().options;
        assert_eq!(read.text_editing.tab_stop, 4);
        assert!(!read.file_operations.confirm_delete);
        assert_eq!(
            read.appearance
                .palettes
                .group(ColorGroup::Text)
                .table(true)
                .get("same_line"),
            Some(Rgb::new(0x10, 0x20, 0x30))
        );
        assert_eq!(read.schema_version, OPTIONS_SCHEMA_VERSION);
    }

    #[test]
    fn a_document_that_cannot_be_parsed_is_moved_aside_rather_than_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(&dir);
        std::fs::write(&file, "{not json").unwrap();
        let load = ProgramOptions::load(&file).unwrap();
        let backup = load.recovered_backup.unwrap();
        assert!(backup.exists());
        assert_eq!(*load.options, ProgramOptions::default());
    }

    #[test]
    fn a_document_that_is_not_utf8_is_moved_aside_rather_than_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(&dir);
        let damaged = b"{\"schemaVersion\": 1, \"colors\": \"\xff\"}".to_vec();
        std::fs::write(&file, &damaged).unwrap();
        let load = ProgramOptions::load(&file).unwrap();
        let backup = load.recovered_backup.unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), damaged);
        assert!(!file.exists());
    }

    #[test]
    fn a_newer_document_loads_and_keeps_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(&dir);
        let mut options = ProgramOptions::default();
        options.save_replacing(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let text = text.replace(
            "\"schemaVersion\": 1",
            &format!("\"schemaVersion\": {}", OPTIONS_SCHEMA_VERSION + 5),
        );
        std::fs::write(&file, text).unwrap();

        let load = ProgramOptions::load(&file).unwrap();
        assert_eq!(load.newer_schema, Some(OPTIONS_SCHEMA_VERSION + 5));
        assert!(load.options.is_newer_schema());
        let mut options = *load.options;
        options.save(&file).unwrap();
        let after = ProgramOptions::load(&file).unwrap();
        assert_eq!(after.newer_schema, Some(OPTIONS_SCHEMA_VERSION + 5));
    }

    #[test]
    fn a_save_over_a_replaced_document_is_refused_and_a_merging_save_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(&dir);
        let mut first = ProgramOptions::default();
        first.save_replacing(&file).unwrap();
        let mut second = *ProgramOptions::load(&file).unwrap().options;

        first.tweaks.use_ipv6 = false;
        first.save_replacing(&file).unwrap();

        second.text_editing.tab_stop = 3;
        assert!(second.save(&file).is_err());
        assert!(second.save_merging(&file).unwrap());
        let read = ProgramOptions::load(&file).unwrap().options;
        assert_eq!(read.text_editing.tab_stop, 3);
    }

    #[test]
    fn a_merging_save_keeps_a_field_this_build_has_no_place_for() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(&dir);
        let mut first = ProgramOptions::default();
        first.save_replacing(&file).unwrap();
        let mut second = *ProgramOptions::load(&file).unwrap().options;

        first.tweaks.unknown.insert(
            "soundScheme".to_owned(),
            serde_json::Value::String("quiet".to_owned()),
        );
        first.save_replacing(&file).unwrap();

        second.tweaks.use_ipv6 = false;
        second.save_merging(&file).unwrap();
        let read = ProgramOptions::load(&file).unwrap().options;
        assert_eq!(
            read.tweaks
                .unknown
                .get("soundScheme")
                .and_then(|v| v.as_str()),
            Some("quiet")
        );
        assert!(!read.tweaks.use_ipv6);
    }

    #[test]
    fn restoring_the_options_category_leaves_the_colors_and_the_shortcuts_alone() {
        let mut options = ProgramOptions::default();
        options.text_editing.tab_stop = 3;
        options
            .appearance
            .palettes
            .group_mut(ColorGroup::Text)
            .table_mut(true)
            .set("same_line", Rgb::new(1, 2, 3));
        options
            .commands
            .set_shortcuts("text", "FindNext", vec!["F8".to_owned()]);

        options.restore(&RestoreSelection {
            categories: vec![RestoreCategory::ProgramOptions],
            ..RestoreSelection::default()
        });
        assert_eq!(
            options.text_editing.tab_stop,
            ProgramOptions::default().text_editing.tab_stop
        );
        assert!(!options
            .appearance
            .palettes
            .group(ColorGroup::Text)
            .is_empty());
        assert!(options.commands.shortcuts("text", "FindNext").is_some());
    }

    #[test]
    fn each_of_the_other_two_categories_restores_only_its_own_page() {
        let mut options = ProgramOptions::default();
        options.text_editing.tab_stop = 3;
        options.appearance.fonts.editor_point_size = 40.0;
        options
            .commands
            .set_shortcuts("text", "FindNext", vec!["F8".to_owned()]);

        options.restore(&RestoreSelection {
            categories: vec![RestoreCategory::ThemeColorsFonts],
            ..RestoreSelection::default()
        });
        assert!(
            (options.appearance.fonts.editor_point_size
                - ProgramOptions::default().appearance.fonts.editor_point_size)
                .abs()
                < f32::EPSILON
        );
        assert_eq!(options.text_editing.tab_stop, 3);

        options.restore(&RestoreSelection {
            categories: vec![RestoreCategory::ToolbarsShortcutsMenus],
            ..RestoreSelection::default()
        });
        assert_eq!(options.commands.shortcuts("text", "FindNext"), None);
        assert_eq!(options.text_editing.tab_stop, 3);
    }

    #[test]
    fn the_wizard_needs_a_second_page_only_for_the_three_listed_categories() {
        let mut selection = RestoreSelection {
            categories: vec![RestoreCategory::ProgramOptions],
            ..RestoreSelection::default()
        };
        assert!(!selection.needs_second_page());
        selection.categories.push(RestoreCategory::Sessions);
        assert!(selection.needs_second_page());
    }

    #[test]
    fn every_category_states_whether_this_build_carries_it_out() {
        let carried: Vec<&str> = RestoreCategory::ALL
            .iter()
            .filter(|category| category.is_carried_out())
            .map(|category| category.label())
            .collect();
        assert_eq!(carried.len(), 4, "{carried:?}");
    }
}
