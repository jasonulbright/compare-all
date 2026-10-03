//! Where a run keeps its write ahead records.

use std::path::PathBuf;

use ca_session::store::SettingsPaths;

/// The archive masks stated on the Archive Types options page, applied over
/// the defaults.
///
/// The options document is read and never written here. A missing or
/// unreadable document leaves the defaults in force, because a script must
/// still run on a machine where the program was never opened.
#[must_use]
pub fn stored_archive_types() -> ca_fs::ArchiveTypes {
    let file = settings_directory().join(ca_session::options::OPTIONS_FILE);
    let options = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<ca_session::options::ProgramOptions>(&text).ok())
        .unwrap_or_default();
    archive_types_from(&options.archives)
}

/// The archive masks of an options document, applied over the defaults.
#[must_use]
pub fn archive_types_from(stored: &ca_session::options::ArchiveOptions) -> ca_fs::ArchiveTypes {
    let mut types = ca_fs::ArchiveTypes::default();
    types.apply_stored(
        stored
            .masks
            .iter()
            .map(|(id, text)| (id.as_str(), text.as_str())),
    );
    types
}

/// Name of the folder the journals live in, below the settings folder.
pub const JOURNAL_FOLDER: &str = "journals";

/// The settings folder for this run.
///
/// `COMPARE_ALL_SETTINGS_DIR` replaces the per-user folder while it holds a
/// value, which is what keeps a test out of the real per-user folder. An
/// environment that names no home yields the folder of this run that
/// [`SettingsPaths::state_directory`] describes, never the working directory.
#[must_use]
pub fn settings_directory() -> PathBuf {
    SettingsPaths::state_directory().path
}

/// The folder the file operations write their journals to.
#[must_use]
pub fn journal_directory() -> PathBuf {
    settings_directory().join(JOURNAL_FOLDER)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::archive_types_from;
    use ca_fs::ArchiveFormat;

    #[test]
    fn stored_masks_reach_the_types_a_script_opens_with() {
        let mut stored = ca_session::options::ArchiveOptions::default();
        stored.masks.insert("cab".to_owned(), String::new());
        stored
            .masks
            .insert("zip".to_owned(), "*.zip;*.pkg".to_owned());
        let types = archive_types_from(&stored);
        assert!(!types.is_enabled(ArchiveFormat::Cab));
        assert_eq!(types.format_for_name("a.pkg"), Some(ArchiveFormat::Zip));
        assert!(types.is_enabled(ArchiveFormat::Rpm));
    }
}
