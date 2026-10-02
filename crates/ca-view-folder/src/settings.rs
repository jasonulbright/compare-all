//! The one conversion from stored settings to the options the folder engine
//! takes.
//!
//! Every value a scan and a comparison run under comes through here, so a
//! toolbar control and a settings page edit the same thing.

use ca_fs::compare::{AlignmentOptions, AlignmentOverride};
use ca_fs::criteria::{
    AttributeComparison, CompareOptions, ContentMethod, ContentTests, QuickTests,
};
use ca_fs::filter::{
    AttributeKind, CaseSensitivity, ContentFilter, FilterTime, OtherFilter, OtherFilters,
    UnixFileType,
};
use ca_fs::ops::plan::OperationOptions;
use ca_fs::scan::ScanOptions;
use ca_fs::ArchiveTypes;
use ca_session::settings::folder::{
    AlignmentOverrideItem, ArchiveHandling, ContentComparison, FolderComparisonSettings,
    FolderHandlingSettings, FolderMiscSettings, NameFilterSettings, OtherFilterItem,
    OtherFilterSettings, SyncMethod,
};
use ca_session::settings::{FolderCompareSettings, FolderSyncSettings};

use crate::sync_mode::Method;

/// How the view itself behaves, as opposed to how the engines compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlingOptions {
    /// Show the top level first and fill the subfolders in behind it.
    pub background_subfolders: bool,
    /// Open every folder once the comparison lands.
    pub expand_on_load: bool,
    /// Limit that expansion to folders holding a difference.
    pub expand_only_with_differences: bool,
    /// Minutes between automatic refreshes; `None` never refreshes.
    pub refresh_minutes: Option<u32>,
}

impl Default for HandlingOptions {
    fn default() -> Self {
        Self {
            background_subfolders: true,
            expand_on_load: false,
            expand_only_with_differences: true,
            refresh_minutes: None,
        }
    }
}

/// Everything a folder comparison runs under, derived from stored settings.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// How each side is walked.
    pub scan: ScanOptions,
    /// How names on the two sides are paired.
    pub alignment: AlignmentOptions,
    /// Which tests decide whether a pair counts as changed.
    pub compare: CompareOptions,
    /// Criteria excluding items for a reason other than their name.
    pub other_filters: OtherFilters,
    /// How the view presents and refreshes the result.
    pub handling: HandlingOptions,
    /// What a copy or a move carries across.
    pub operations: OperationOptions,
    /// Formats this session turns on regardless of the global state.
    pub enabled_formats: Vec<String>,
    /// Formats this session turns off regardless of the global state.
    pub disabled_formats: Vec<String>,
    /// Name masks, as the filter engine reads them: one line per mask.
    pub include_files: Vec<String>,
    /// Masks naming files the session leaves out.
    pub exclude_files: Vec<String>,
    /// Masks naming folders the session takes in.
    pub include_folders: Vec<String>,
    /// Masks naming folders the session leaves out.
    pub exclude_folders: Vec<String>,
    /// Which container files open as folders, and how.
    pub archives: ArchiveTypes,
}

/// The attribute letters a comparison setting names.
///
/// A letter this build has no flag for is ignored rather than refusing the
/// session.
#[must_use]
pub fn attributes_from(letters: &str) -> AttributeComparison {
    let mut attributes = AttributeComparison::default();
    for letter in letters.chars() {
        match letter.to_ascii_uppercase() {
            'R' => attributes.read_only = true,
            'H' => attributes.hidden = true,
            'S' => attributes.system = true,
            'A' => attributes.archive = true,
            _ => {}
        }
    }
    attributes
}

/// The content test a stored choice names, and whether one runs at all.
#[must_use]
pub fn content_method(choice: &ContentComparison) -> Option<ContentMethod> {
    match choice {
        ContentComparison::Crc => Some(ContentMethod::Crc32),
        ContentComparison::Binary => Some(ContentMethod::Binary),
        ContentComparison::RulesBased => Some(ContentMethod::Rules),
        // A test this build does not understand reads nothing, as a session
        // asking for none does: guessing which of the three was meant would
        // change the result.
        _ => None,
    }
}

/// The tests a stored comparison group states.
#[must_use]
pub fn quick_tests(comparison: &FolderComparisonSettings) -> QuickTests {
    QuickTests {
        size: comparison.compare_size,
        timestamp: comparison.compare_timestamps,
        tolerance_seconds: comparison.timestamp_tolerance_seconds,
        ignore_daylight_saving: comparison.ignore_daylight_saving,
        ignore_timezone: comparison.ignore_timezone,
        filename_case: comparison.compare_filename_case,
        attributes: if comparison.compare_attributes {
            attributes_from(&comparison.compared_attributes)
        } else {
            AttributeComparison::default()
        },
        unix_permissions: comparison.compare_permissions,
        owner: comparison.compare_owner,
        group: comparison.compare_group,
        version: comparison.compare_versions,
    }
}

/// The content pass a stored comparison group states.
#[must_use]
pub fn content_tests(comparison: &FolderComparisonSettings) -> ContentTests {
    let method = content_method(&comparison.content_comparison);
    ContentTests {
        enabled: method.is_some(),
        method: method.unwrap_or_default(),
        skip_if_quick_same: comparison.skip_content_if_quick_tests_match,
        override_quick: comparison.override_quick_test_results,
        binary_threshold_bytes: comparison.binary_size_threshold_bytes,
        ..ContentTests::default()
    }
}

/// How the tree is walked, from a stored handling group.
#[must_use]
pub fn scan_options(handling: &FolderHandlingSettings) -> ScanOptions {
    ScanOptions {
        follow_links: handling.follow_symbolic_links,
        ..ScanOptions::default()
    }
}

/// How the view presents and refreshes the result.
#[must_use]
pub fn handling_options(handling: &FolderHandlingSettings) -> HandlingOptions {
    HandlingOptions {
        background_subfolders: handling.scan_subfolders_in_background,
        expand_on_load: handling.expand_subfolders_on_load,
        expand_only_with_differences: handling.expand_only_folders_with_differences,
        refresh_minutes: handling
            .automatic_refresh
            .then_some(handling.automatic_refresh_minutes.max(1)),
    }
}

/// What a copy or a move carries across, from a stored handling group.
#[must_use]
pub fn operation_options(handling: &FolderHandlingSettings) -> OperationOptions {
    OperationOptions {
        preserve_created: handling.copy_creation_dates,
        preserve_attributes: handling.copy_file_permissions,
        touch_source_after_copy: handling.touch_local_files_on_upload,
        ..OperationOptions::default()
    }
}

/// How names are paired, from the comparison, handling and alignment groups.
#[must_use]
pub fn alignment_options(
    comparison: &FolderComparisonSettings,
    handling: &FolderHandlingSettings,
    overrides: &[AlignmentOverrideItem],
) -> AlignmentOptions {
    AlignmentOptions {
        case: if comparison.compare_filename_case {
            CaseSensitivity::Sensitive
        } else {
            CaseSensitivity::Platform
        },
        align_different_extensions: comparison.align_different_extensions,
        align_unicode_normalization: comparison.align_different_normalization,
        overrides: overrides
            .iter()
            .filter(|item| !item.left.trim().is_empty() && !item.right.trim().is_empty())
            .map(|item| AlignmentOverride {
                left: item.left.trim().to_owned(),
                right: item.right.trim().to_owned(),
                limit_to_folder: item.limit_to_folder.trim().to_owned(),
            })
            .collect(),
        scan_top_level_orphans: handling.scan_top_level_orphans,
    }
}

/// The attribute a filter item's letter names.
fn attribute_kind(letter: &str) -> Option<AttributeKind> {
    match letter.trim().chars().next()?.to_ascii_uppercase() {
        'R' => Some(AttributeKind::ReadOnly),
        'H' => Some(AttributeKind::Hidden),
        'S' => Some(AttributeKind::System),
        'A' => Some(AttributeKind::Archive),
        _ => None,
    }
}

/// The non-name criteria a stored filter group states.
///
/// An item this build cannot act on is left out rather than applied loosely,
/// because a filter that excludes the wrong entries is worse than one that
/// excludes none.
#[must_use]
pub fn other_filters(filters: &OtherFilterSettings) -> OtherFilters {
    let mut out = OtherFilters {
        exclude_protected_system: filters.exclude_protected_system_files,
        ..OtherFilters::default()
    };
    for item in &filters.items {
        match item {
            OtherFilterItem::Modified {
                older_than,
                days_ago,
                absolute_seconds,
                ..
            } => {
                let time = if let Some(days) = days_ago {
                    FilterTime::DaysAgo(*days)
                } else {
                    let Some(seconds) = absolute_seconds else {
                        continue;
                    };
                    let Some(time) = ca_fs::filter::from_unix_seconds(*seconds) else {
                        continue;
                    };
                    FilterTime::Absolute(time)
                };
                out.items.push(if *older_than {
                    OtherFilter::ModifiedOlderThan(time)
                } else {
                    OtherFilter::ModifiedNewerThan(time)
                });
            }
            OtherFilterItem::Size {
                smaller_than,
                bytes,
                ..
            } => out.items.push(if *smaller_than {
                OtherFilter::SmallerThan(*bytes)
            } else {
                OtherFilter::LargerThan(*bytes)
            }),
            OtherFilterItem::Attribute {
                is_not_set,
                attribute,
                ..
            } => {
                let Some(kind) = attribute_kind(attribute) else {
                    continue;
                };
                out.items.push(if *is_not_set {
                    OtherFilter::AttributeNotSet(kind)
                } else {
                    OtherFilter::AttributeSet(kind)
                });
            }
            OtherFilterItem::UnixFileType {
                is_not, file_type, ..
            } => {
                let Some(kind) = UnixFileType::parse(file_type) else {
                    continue;
                };
                out.items.push(if *is_not {
                    OtherFilter::UnixFileTypeIsNot(kind)
                } else {
                    OtherFilter::UnixFileTypeIs(kind)
                });
            }
            OtherFilterItem::Content {
                not_containing,
                text,
                ..
            } => {
                if text.trim().is_empty() {
                    continue;
                }
                out.content.push(ContentFilter {
                    text: text.clone(),
                    not_containing: *not_containing,
                });
            }
            // A criterion this build does not understand excludes nothing.
            _ => {}
        }
    }
    out
}

/// Which container files open as folders, from the session's Handling choice
/// and the masks of the Archive Types options page.
///
/// A side named in the session counts as opened, so the choice to show
/// archives as folders opens it. A choice this build does not understand
/// leaves archives as files.
#[must_use]
pub fn archive_types(
    handling: &ArchiveHandling,
    stored: Option<&ca_session::options::ArchiveOptions>,
) -> ArchiveTypes {
    let mut types = ArchiveTypes::default();
    if let Some(stored) = stored {
        types.apply_stored(
            stored
                .masks
                .iter()
                .map(|(id, text)| (id.as_str(), text.as_str())),
        );
    }
    types.set_handling(match handling {
        ArchiveHandling::AsFolders => ca_fs::ArchiveHandling::AsFoldersOnceOpened,
        _ => ca_fs::ArchiveHandling::AsFiles,
    });
    types
}
/// The options the stored groups of a folder session state.
#[must_use]
pub fn options_from(
    comparison: &FolderComparisonSettings,
    handling: &FolderHandlingSettings,
    name_filters: &NameFilterSettings,
    filters: &OtherFilterSettings,
    misc: &FolderMiscSettings,
) -> EngineOptions {
    EngineOptions {
        scan: scan_options(handling),
        alignment: alignment_options(comparison, handling, &misc.alignment_overrides),
        compare: CompareOptions {
            quick: quick_tests(comparison),
            content: content_tests(comparison),
        },
        other_filters: other_filters(filters),
        handling: handling_options(handling),
        operations: operation_options(handling),
        enabled_formats: misc.enabled_formats.clone(),
        disabled_formats: misc.disabled_formats.clone(),
        include_files: name_filters.include_files.clone(),
        exclude_files: name_filters.exclude_files.clone(),
        include_folders: name_filters.include_folders.clone(),
        exclude_folders: name_filters.exclude_folders.clone(),
        archives: archive_types(&handling.archive_handling, None),
    }
}

/// The four mask lists as the one filter line the scan reads.
///
/// The line is what the filter engine parses: masks separated by semicolons, a
/// leading minus for an exclusion and a trailing separator for a folder.
#[must_use]
pub fn name_filter_text(filters: &NameFilterSettings) -> String {
    let mut parts: Vec<String> = Vec::new();
    for mask in &filters.include_files {
        parts.push(mask.clone());
    }
    for mask in &filters.exclude_files {
        parts.push(format!("-{mask}"));
    }
    for mask in &filters.include_folders {
        parts.push(format!("{mask}/"));
    }
    for mask in &filters.exclude_folders {
        parts.push(format!("-{mask}/"));
    }
    parts.join(";")
}

/// The options of a whole folder comparison session.
#[must_use]
pub fn options_of(settings: &ca_session::settings::FolderCompareSettings) -> EngineOptions {
    options_from(
        &settings.comparison,
        &settings.handling,
        &settings.name_filters,
        &settings.other_filters,
        &settings.misc,
    )
}

/// The options of a whole folder synchronization session.
///
/// A synchronization session carries no alignment rules or format lists of its
/// own, so those stay at their stored defaults.
#[must_use]
pub fn sync_options_of(settings: &ca_session::settings::FolderSyncSettings) -> EngineOptions {
    options_from(
        &settings.comparison,
        &settings.handling,
        &settings.name_filters,
        &settings.other_filters,
        &FolderMiscSettings::default(),
    )
}

/// The method a stored choice names.
///
/// A method this build does not understand yields nothing, and the view keeps
/// the method it runs under.
#[must_use]
pub fn sync_method(stored: &SyncMethod) -> Option<Method> {
    match stored {
        SyncMethod::UpdateLeft => Some(Method::UpdateLeft),
        SyncMethod::UpdateRight => Some(Method::UpdateRight),
        SyncMethod::UpdateBoth => Some(Method::UpdateBoth),
        SyncMethod::MirrorToLeft => Some(Method::MirrorToLeft),
        SyncMethod::MirrorToRight => Some(Method::MirrorToRight),
        _ => None,
    }
}

/// The stored choice that names a method.
#[must_use]
pub fn stored_sync_method(method: Method) -> SyncMethod {
    match method {
        Method::UpdateLeft => SyncMethod::UpdateLeft,
        Method::UpdateRight => SyncMethod::UpdateRight,
        Method::UpdateBoth => SyncMethod::UpdateBoth,
        Method::MirrorToLeft => SyncMethod::MirrorToLeft,
        Method::MirrorToRight => SyncMethod::MirrorToRight,
    }
}

/// The comparison settings the folder view of a synchronization runs under.
///
/// The groups the two kinds share come from `sync`. The groups a
/// synchronization does not carry come from `kept`.
#[must_use]
pub fn compare_settings_of(
    sync: &FolderSyncSettings,
    kept: &FolderCompareSettings,
) -> FolderCompareSettings {
    let mut settings = kept.clone();
    settings.specs = sync.specs.clone();
    settings.comparison = sync.comparison.clone();
    settings.handling = sync.handling.clone();
    settings.name_filters = sync.name_filters.clone();
    settings.other_filters = sync.other_filters.clone();
    settings
}

/// The settings of a synchronization as its view runs it.
///
/// The groups the two kinds share come from `folder`. The method is
/// `method`, the one the view chose; `None` keeps the method `applied`
/// names, which can be one this build does not know. Every other value comes
/// from `applied`.
#[must_use]
pub fn sync_settings_of(
    folder: FolderCompareSettings,
    method: Option<Method>,
    applied: &FolderSyncSettings,
) -> FolderSyncSettings {
    let mut settings = applied.clone();
    settings.specs = folder.specs;
    settings.comparison = folder.comparison;
    settings.handling = folder.handling;
    settings.name_filters = folder.name_filters;
    settings.other_filters = folder.other_filters;
    if let Some(method) = method {
        settings.sync.method = stored_sync_method(method);
    }
    settings
}

/// The options of a whole folder merge session.
///
/// A merge session carries no alignment rules or format lists of its own, so
/// those stay at their stored defaults.
#[must_use]
pub fn merge_options_of(settings: &ca_session::settings::FolderMergeSettings) -> EngineOptions {
    options_from(
        &settings.comparison,
        &settings.handling,
        &settings.name_filters,
        &settings.other_filters,
        &FolderMiscSettings::default(),
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{attributes_from, content_method, options_of, quick_tests};
    use ca_fs::criteria::ContentMethod;
    use ca_fs::filter::CaseSensitivity;
    use ca_session::settings::folder::{ContentComparison, FolderComparisonSettings};
    use ca_session::settings::FolderCompareSettings;

    /// Each quick test flag reaches exactly the engine field it names.
    #[test]
    fn every_quick_test_reaches_its_own_field() {
        type Case = (
            fn(&mut FolderComparisonSettings),
            fn(&ca_fs::criteria::QuickTests) -> bool,
        );
        let cases: &[Case] = &[
            (|group| group.compare_size = false, |quick| !quick.size),
            (
                |group| group.compare_timestamps = false,
                |quick| !quick.timestamp,
            ),
            (
                |group| group.ignore_daylight_saving = true,
                |quick| quick.ignore_daylight_saving,
            ),
            (
                |group| group.ignore_timezone = true,
                |quick| quick.ignore_timezone,
            ),
            (
                |group| group.compare_filename_case = true,
                |quick| quick.filename_case,
            ),
            (
                |group| group.compare_permissions = true,
                |quick| quick.unix_permissions,
            ),
            (|group| group.compare_owner = true, |quick| quick.owner),
            (|group| group.compare_group = true, |quick| quick.group),
            (|group| group.compare_versions = true, |quick| quick.version),
        ];
        for (index, (edit, read)) in cases.iter().enumerate() {
            let mut group = FolderComparisonSettings::default();
            edit(&mut group);
            assert!(read(&quick_tests(&group)), "case {index}");
        }
    }

    #[test]
    fn the_timestamp_tolerance_reaches_the_engine_unchanged() {
        let mut group = FolderComparisonSettings::default();
        group.timestamp_tolerance_seconds = 45;
        assert_eq!(quick_tests(&group).tolerance_seconds, 45);
    }

    #[test]
    fn every_content_test_reaches_its_own_method() {
        assert_eq!(content_method(&ContentComparison::None), None);
        assert_eq!(
            content_method(&ContentComparison::Crc),
            Some(ContentMethod::Crc32)
        );
        assert_eq!(
            content_method(&ContentComparison::Binary),
            Some(ContentMethod::Binary)
        );
        assert_eq!(
            content_method(&ContentComparison::RulesBased),
            Some(ContentMethod::Rules)
        );
        assert_eq!(
            content_method(&ContentComparison::Unknown(serde_json::json!("future"))),
            None,
            "a test this build cannot run reads nothing"
        );
    }

    #[test]
    fn the_content_group_reaches_the_content_tests() {
        let mut settings = FolderCompareSettings::default();
        settings.comparison.content_comparison = ContentComparison::Binary;
        settings.comparison.skip_content_if_quick_tests_match = false;
        settings.comparison.override_quick_test_results = false;
        let content = options_of(&settings).compare.content;
        assert!(content.enabled);
        assert_eq!(content.method, ContentMethod::Binary);
        assert!(!content.skip_if_quick_same);
        assert!(!content.override_quick);
    }

    #[test]
    fn attribute_letters_reach_their_own_flags() {
        let attributes = attributes_from("rHs");
        assert!(attributes.read_only);
        assert!(attributes.hidden);
        assert!(attributes.system);
        assert!(!attributes.archive);
        assert!(
            attributes_from("Z?").is_empty(),
            "a letter with no flag is ignored"
        );
    }

    #[test]
    fn attributes_take_part_only_while_the_test_is_on() {
        let mut group = FolderComparisonSettings::default();
        group.compared_attributes = "RHSA".to_owned();
        assert!(quick_tests(&group).attributes.is_empty());
        group.compare_attributes = true;
        assert!(!quick_tests(&group).attributes.is_empty());
    }

    #[test]
    fn the_name_alignment_settings_reach_the_alignment_options() {
        let mut settings = FolderCompareSettings::default();
        settings.comparison.align_different_extensions = true;
        settings.comparison.align_different_normalization = true;
        settings.comparison.compare_filename_case = true;
        let alignment = options_of(&settings).alignment;
        assert!(alignment.align_different_extensions);
        assert!(alignment.align_unicode_normalization);
        assert_eq!(alignment.case, CaseSensitivity::Sensitive);
    }

    #[test]
    fn following_links_reaches_the_scan() {
        let mut settings = FolderCompareSettings::default();
        assert!(!options_of(&settings).scan.follow_links);
        settings.handling.follow_symbolic_links = true;
        assert!(options_of(&settings).scan.follow_links);
    }

    #[test]
    fn the_name_masks_reach_the_options_unchanged() {
        let mut settings = FolderCompareSettings::default();
        settings.name_filters.exclude_files = vec!["*.tmp".to_owned(), "*.bak".to_owned()];
        settings.name_filters.include_folders = vec!["src".to_owned()];
        let options = options_of(&settings);
        assert_eq!(options.exclude_files, vec!["*.tmp", "*.bak"]);
        assert_eq!(options.include_folders, vec!["src"]);
        assert!(options.include_files.is_empty());
    }

    #[test]
    fn the_four_mask_lists_reach_one_filter_line() {
        let mut filters = ca_session::settings::folder::NameFilterSettings::default();
        filters.include_files = vec!["*.rs".to_owned()];
        filters.exclude_files = vec!["*.tmp".to_owned()];
        filters.include_folders = vec!["src".to_owned()];
        filters.exclude_folders = vec!["target".to_owned()];
        let line = super::name_filter_text(&filters);
        assert_eq!(line, "*.rs;-*.tmp;src/;-target/");
        let masks = ca_fs::filter::NameFilters::parse(&line);
        assert_eq!(masks.include.len(), 2, "both inclusions were read");
        assert_eq!(masks.exclude.len(), 2, "both exclusions were read");
    }

    #[test]
    fn no_mask_produces_an_empty_filter_line() {
        assert!(super::name_filter_text(
            &ca_session::settings::folder::NameFilterSettings::default()
        )
        .is_empty());
    }

    #[test]
    fn the_binary_threshold_reaches_the_content_tests() {
        let mut settings = FolderCompareSettings::default();
        settings.comparison.content_comparison = ContentComparison::RulesBased;
        settings.comparison.binary_size_threshold_bytes = 123;
        assert_eq!(
            options_of(&settings).compare.content.binary_threshold_bytes,
            123
        );
    }

    #[test]
    fn every_handling_flag_reaches_the_option_it_names() {
        let mut settings = FolderCompareSettings::default();
        settings.handling.scan_subfolders_in_background = false;
        settings.handling.scan_top_level_orphans = true;
        settings.handling.expand_subfolders_on_load = true;
        settings.handling.expand_only_folders_with_differences = false;
        settings.handling.copy_creation_dates = true;
        settings.handling.copy_file_permissions = true;
        settings.handling.touch_local_files_on_upload = true;
        settings.handling.automatic_refresh = true;
        settings.handling.automatic_refresh_minutes = 9;

        let options = options_of(&settings);
        assert!(!options.handling.background_subfolders);
        assert!(options.alignment.scan_top_level_orphans);
        assert!(options.handling.expand_on_load);
        assert!(!options.handling.expand_only_with_differences);
        assert!(options.operations.preserve_created);
        assert!(options.operations.preserve_attributes);
        assert!(options.operations.touch_source_after_copy);
        assert_eq!(options.handling.refresh_minutes, Some(9));
    }

    #[test]
    fn refresh_minutes_are_absent_while_the_refresh_is_off() {
        let settings = FolderCompareSettings::default();
        assert_eq!(options_of(&settings).handling.refresh_minutes, None);
    }

    #[test]
    fn every_other_filter_item_reaches_the_engine() {
        use ca_session::settings::folder::OtherFilterItem as Item;
        let empty = std::collections::BTreeMap::new;
        let mut settings = FolderCompareSettings::default();
        settings.other_filters.exclude_protected_system_files = false;
        settings.other_filters.items = vec![
            Item::Modified {
                older_than: true,
                days_ago: Some(3),
                absolute_seconds: None,
                unknown: empty(),
            },
            Item::Size {
                smaller_than: true,
                bytes: 64,
                unknown: empty(),
            },
            Item::Attribute {
                is_not_set: false,
                attribute: "H".to_owned(),
                unknown: empty(),
            },
            Item::UnixFileType {
                is_not: false,
                file_type: "symlink".to_owned(),
                unknown: empty(),
            },
            Item::Content {
                not_containing: true,
                text: "marker".to_owned(),
                unknown: empty(),
            },
        ];
        let filters = options_of(&settings).other_filters;
        assert!(!filters.exclude_protected_system);
        assert_eq!(filters.items.len(), 4, "{:?}", filters.items);
        assert!(matches!(
            filters.items[0],
            ca_fs::OtherFilter::ModifiedOlderThan(ca_fs::FilterTime::DaysAgo(3))
        ));
        assert!(matches!(
            filters.items[1],
            ca_fs::OtherFilter::SmallerThan(64)
        ));
        assert!(matches!(
            filters.items[2],
            ca_fs::OtherFilter::AttributeSet(ca_fs::AttributeKind::Hidden)
        ));
        assert!(matches!(
            filters.items[3],
            ca_fs::OtherFilter::UnixFileTypeIs(ca_fs::UnixFileType::Symlink)
        ));
        assert_eq!(filters.content.len(), 1);
        assert!(filters.content[0].not_containing);
    }

    #[test]
    fn a_filter_item_this_build_cannot_act_on_is_left_out() {
        use ca_session::settings::folder::OtherFilterItem as Item;
        let mut settings = FolderCompareSettings::default();
        settings.other_filters.items = vec![
            Item::Attribute {
                is_not_set: false,
                attribute: "Z".to_owned(),
                unknown: std::collections::BTreeMap::new(),
            },
            Item::Unknown(serde_json::json!({"kind": "future"})),
        ];
        assert!(options_of(&settings).other_filters.items.is_empty());
    }

    #[test]
    fn an_unrepresentable_stored_filter_date_is_left_out() {
        use ca_session::settings::folder::OtherFilterItem as Item;

        let mut settings = FolderCompareSettings::default();
        settings.other_filters.items = vec![Item::Modified {
            older_than: true,
            days_ago: None,
            absolute_seconds: Some(1_000_000_000_000),
            unknown: std::collections::BTreeMap::new(),
        }];

        let options = options_of(&settings);
        if std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(1_000_000_000_000))
            .is_none()
        {
            assert!(options.other_filters.items.is_empty());
        } else {
            assert_eq!(options.other_filters.items.len(), 1);
        }
    }

    #[test]
    fn the_alignment_rules_and_format_lists_reach_the_options() {
        let mut settings = FolderCompareSettings::default();
        settings.misc.alignment_overrides = vec![
            ca_session::settings::folder::AlignmentOverrideItem {
                left: " a.* ".to_owned(),
                right: " b.* ".to_owned(),
                limit_to_folder: "src".to_owned(),
                ..Default::default()
            },
            ca_session::settings::folder::AlignmentOverrideItem::default(),
        ];
        settings.misc.enabled_formats = vec!["Rust".to_owned()];
        settings.misc.disabled_formats = vec!["Python".to_owned()];
        let options = options_of(&settings);
        assert_eq!(
            options.alignment.overrides.len(),
            1,
            "a blank rule is left out"
        );
        assert_eq!(options.alignment.overrides[0].left, "a.*");
        assert_eq!(options.alignment.overrides[0].limit_to_folder, "src");
        assert_eq!(options.enabled_formats, vec!["Rust"]);
        assert_eq!(options.disabled_formats, vec!["Python"]);
    }

    #[test]
    fn the_stored_defaults_produce_the_engine_defaults() {
        let options = options_of(&FolderCompareSettings::default());
        assert_eq!(
            options.compare.quick,
            ca_fs::criteria::QuickTests::default()
        );
        assert!(!options.compare.content.enabled);
    }

    /// Every method reads back from its stored form, and a method of another
    /// build names none.
    #[test]
    fn every_sync_method_reads_back_from_its_stored_form() {
        use super::{stored_sync_method, sync_method};
        use ca_session::settings::folder::SyncMethod;

        for method in crate::sync_mode::PRESETS {
            assert_eq!(sync_method(&stored_sync_method(method)), Some(method));
        }
        assert_eq!(
            sync_method(&SyncMethod::Unknown(serde_json::json!("custom"))),
            None
        );
    }
}
