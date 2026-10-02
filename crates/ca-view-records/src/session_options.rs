//! The one conversion from stored session settings to the rules a record
//! comparison runs under.

use crate::flavor::Flavor;
use ca_records::AlignOptions;
use ca_session::settings::SessionSettings;

/// Group names the media engine writes its tag groups under.
pub const MEDIA_TAG_GROUPS: &[&str] = &["ID3v1", "ID3v2", "Tags"];
/// Group name the media engine writes its stream facts under.
pub const MEDIA_STREAM_GROUP: &str = "Audio";
/// Record name the media engine writes the play time under.
pub const MEDIA_DURATION: &str = "Duration";
/// Group name the version engine writes the string tables under.
pub const VERSION_STRING_GROUP: &str = "StringFileInfo";
/// Names the version engine gives the file version, fixed and string forms.
pub const VERSION_FILE_VERSION: &[&str] = &["File Version", "FileVersion"];
/// Names the version engine gives the product version, fixed and string forms.
pub const VERSION_PRODUCT_VERSION: &[&str] = &["Product Version", "ProductVersion"];

/// What a comparison runs under, beyond the two trees.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rules {
    /// Rules the engine applies while it aligns the two trees.
    ///
    /// `unimportant_fields` holds record names, and group paths joined to a
    /// record name by a backslash.
    pub align: AlignOptions,
    /// Group paths whose records, at any depth, are unimportant.
    pub unimportant_groups: Vec<String>,
    /// Two play times closer than this, in milliseconds, count as the same.
    pub duration_tolerance_ms: u64,
}

impl Rules {
    /// True when a record in group `path` is unimportant by its group.
    #[must_use]
    pub fn group_is_unimportant(&self, path: &str) -> bool {
        self.unimportant_groups.iter().any(|group| {
            path.eq_ignore_ascii_case(group)
                || (path
                    .get(..group.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(group))
                    && path.as_bytes().get(group.len()) == Some(&b'\\'))
        })
    }
}

/// The rules a stored session states.
///
/// Settings of another kind than `flavor` leave the default rules, so a caller
/// never has to match the kind first.
#[must_use]
pub fn rules_from(flavor: Flavor, settings: &SessionSettings) -> Rules {
    let mut rules = Rules::default();
    match (flavor, settings) {
        (Flavor::Version, SessionSettings::VersionCompare(version)) => {
            let importance = &version.importance;
            let names = &mut rules.align.unimportant_fields;
            if !importance.file_version_important {
                names.extend(VERSION_FILE_VERSION.iter().map(|name| (*name).to_owned()));
            }
            if !importance.product_version_important {
                names.extend(
                    VERSION_PRODUCT_VERSION
                        .iter()
                        .map(|name| (*name).to_owned()),
                );
            }
            if !importance.string_fields_important {
                rules
                    .unimportant_groups
                    .push(VERSION_STRING_GROUP.to_owned());
            }
            names.extend(field_names(&importance.unimportant_fields));
        }
        (Flavor::Media, SessionSettings::MediaCompare(media)) => {
            let importance = &media.importance;
            if !importance.tags_important {
                rules
                    .unimportant_groups
                    .extend(MEDIA_TAG_GROUPS.iter().map(|group| (*group).to_owned()));
            }
            if !importance.stream_important {
                rules.unimportant_groups.push(MEDIA_STREAM_GROUP.to_owned());
            }
            if !importance.duration_important {
                rules
                    .align
                    .unimportant_fields
                    .insert(format!("{MEDIA_STREAM_GROUP}\\{MEDIA_DURATION}"));
            }
            rules.duration_tolerance_ms =
                u64::from(importance.duration_tolerance_seconds).saturating_mul(1000);
            rules
                .align
                .unimportant_fields
                .extend(field_names(&importance.unimportant_tags));
        }
        _ => {}
    }
    rules
}

/// The non-empty, trimmed names of a stored list.
fn field_names(stored: &[String]) -> impl Iterator<Item = String> + '_ {
    stored
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// True when `settings` belongs to the kind `flavor` answers for.
#[must_use]
pub fn belongs_to(flavor: Flavor, settings: &SessionSettings) -> bool {
    matches!(
        (flavor, settings),
        (Flavor::Registry, SessionSettings::RegistryCompare(_))
            | (Flavor::Version, SessionSettings::VersionCompare(_))
            | (Flavor::Media, SessionSettings::MediaCompare(_))
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{belongs_to, rules_from, Rules};
    use crate::flavor::Flavor;
    use ca_session::settings::SessionSettings;

    #[test]
    fn the_defaults_mark_nothing_unimportant() {
        for flavor in [Flavor::Registry, Flavor::Version, Flavor::Media] {
            let settings = SessionSettings::defaults_for(&flavor.kind());
            assert!(belongs_to(flavor, &settings));
            assert_eq!(rules_from(flavor, &settings), Rules::default());
        }
    }

    #[test]
    fn a_media_importance_page_reaches_the_rules() {
        let mut settings = SessionSettings::defaults_for(&Flavor::Media.kind());
        let SessionSettings::MediaCompare(media) = &mut settings else {
            panic!("media defaults are media settings");
        };
        media.importance.tags_important = false;
        media.importance.duration_important = false;
        media.importance.duration_tolerance_seconds = 2;
        media.importance.unimportant_tags = vec![" TCON ".to_owned(), String::new()];
        let rules = rules_from(Flavor::Media, &settings);
        assert!(rules.group_is_unimportant("ID3v2"));
        assert!(!rules.group_is_unimportant("Audio"));
        assert!(rules.align.unimportant_fields.contains("Audio\\Duration"));
        assert!(rules.align.unimportant_fields.contains("TCON"));
        assert!(!rules.align.unimportant_fields.contains(""));
        assert_eq!(rules.duration_tolerance_ms, 2_000);
    }

    #[test]
    fn a_version_importance_page_reaches_the_rules() {
        let mut settings = SessionSettings::defaults_for(&Flavor::Version.kind());
        let SessionSettings::VersionCompare(version) = &mut settings else {
            panic!("version defaults are version settings");
        };
        version.importance.file_version_important = false;
        version.importance.string_fields_important = false;
        let rules = rules_from(Flavor::Version, &settings);
        assert!(rules.align.unimportant_fields.contains("FileVersion"));
        assert!(!rules.align.unimportant_fields.contains("ProductVersion"));
        assert!(rules.group_is_unimportant("StringFileInfo\\040904B0"));
        assert!(!rules.group_is_unimportant("StringFileInfoX"));
    }

    #[test]
    fn settings_of_another_kind_leave_the_defaults() {
        let settings = SessionSettings::defaults_for(&Flavor::Media.kind());
        assert!(!belongs_to(Flavor::Version, &settings));
        assert_eq!(rules_from(Flavor::Version, &settings), Rules::default());
    }
}
