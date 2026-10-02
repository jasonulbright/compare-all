//! Settings owned by the folder session kinds.

use super::common::{SpecsOverride, SpecsSettings};
use super::defaults::{documented, provisional};
use super::macros::{settings_composite, settings_group};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Content test run on a pair of files that the quick tests could not settle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ContentComparison {
    /// Do not read file contents.
    #[default]
    None,
    /// Compare checksums of the whole content.
    Crc,
    /// Compare content byte by byte.
    Binary,
    /// Compare content through its file format, so differences the format
    /// classifies as unimportant do not mark the pair as changed.
    RulesBased,
    /// A test this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// How an archive file is presented in a folder tree.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ArchiveHandling {
    /// Present the archive as a folder whose contents are compared.
    #[default]
    AsFolders,
    /// Present the archive as an ordinary file.
    AsFiles,
    /// A treatment this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// Direction and destructiveness of a synchronization run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SyncMethod {
    /// Copy newer and orphan items from right to left.
    UpdateLeft,
    /// Copy newer and orphan items from left to right.
    #[default]
    UpdateRight,
    /// Copy newer and orphan items in both directions.
    UpdateBoth,
    /// Make the left side match the right, deleting left-side orphans.
    MirrorToLeft,
    /// Make the right side match the left, deleting right-side orphans.
    MirrorToRight,
    /// A method this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// Where a folder merge writes its result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MergeTarget {
    /// Write the merged result into the left folder.
    Left,
    /// Write the merged result into the right folder.
    Right,
    /// Write the merged result into a separate output folder.
    #[default]
    OutputFolder,
    /// A destination this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// One rule pairing differently named items across the two sides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AlignmentOverrideItem {
    /// Left-side name or pattern.
    #[serde(default)]
    pub left: String,
    /// Right-side name or pattern.
    #[serde(default)]
    pub right: String,
    /// Read both fields as replacement-style regular expressions rather than
    /// wildcard masks.
    #[serde(default)]
    pub regular_expression: bool,
    /// Relative path the rule is confined to, empty for the whole tree.
    #[serde(default)]
    pub limit_to_folder: String,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: std::collections::BTreeMap<String, Value>,
}

/// Criterion excluding items for a reason other than their name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum OtherFilterItem {
    /// Exclude by modification time.
    Modified {
        /// Exclude items older than the bound rather than newer.
        #[serde(default, alias = "older_than")]
        older_than: bool,
        /// Bound expressed in whole days before the run, counted from midnight.
        #[serde(default, alias = "days_ago")]
        days_ago: Option<u32>,
        /// Bound expressed as an absolute instant, seconds since the epoch.
        #[serde(default, alias = "absolute_seconds")]
        absolute_seconds: Option<i64>,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Exclude by size in bytes.
    Size {
        /// Exclude items smaller than the bound rather than larger.
        #[serde(default, alias = "smaller_than")]
        smaller_than: bool,
        /// Bound in bytes.
        #[serde(default)]
        bytes: u64,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Exclude by content.
    Content {
        /// Exclude items that do not contain the text rather than those that do.
        #[serde(default, alias = "not_containing")]
        not_containing: bool,
        /// Text searched for.
        #[serde(default)]
        text: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Exclude by a file system attribute letter.
    Attribute {
        /// Exclude items where the attribute is clear rather than set.
        #[serde(default, alias = "is_not_set")]
        is_not_set: bool,
        /// Single-letter attribute name.
        #[serde(default)]
        attribute: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Exclude by file type on a POSIX file system.
    UnixFileType {
        /// Exclude items that are not of the type rather than those that are.
        #[serde(default, alias = "is_not")]
        is_not: bool,
        /// Type name: block, character, symlink, fifo, regular, or socket.
        #[serde(default, alias = "file_type")]
        file_type: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// A criterion this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

settings_group! {
    /// Tests deciding whether a pair of items counts as changed.
    FolderComparisonSettings / FolderComparisonOverride {
        /// Mark a pair changed when the byte sizes differ.
        compare_size: bool = true,
        /// Mark a pair changed when the modification times differ.
        compare_timestamps: bool = true,
        /// Times must differ by more than this many seconds to count.
        timestamp_tolerance_seconds: u32 = provisional::TIMESTAMP_TOLERANCE_SECONDS,
        /// Ignore differences of exactly one hour.
        ignore_daylight_saving: bool = false,
        /// Ignore differences that are whole multiples of an hour.
        ignore_timezone: bool = false,
        /// Mark a pair changed when the capitalization of the name differs.
        compare_filename_case: bool = false,
        /// Mark a pair changed when a compared file system attribute differs.
        compare_attributes: bool = false,
        /// Attribute letters participating in the attribute test.
        compared_attributes: String = String::new(),
        /// Mark a pair changed when POSIX permission bits differ.
        compare_permissions: bool = false,
        /// Mark a pair changed when owner identifiers differ.
        compare_owner: bool = false,
        /// Mark a pair changed when group identifiers differ.
        compare_group: bool = false,
        /// Pair names that match except for their extension.
        align_different_extensions: bool = false,
        /// Pair names that are equivalent under different normalization forms.
        align_different_normalization: bool = false,
        /// Content test run at session load.
        content_comparison: ContentComparison = ContentComparison::None,
        /// Run the content test only where the quick tests already disagree.
        skip_content_if_quick_tests_match: bool = true,
        /// Compare version information resources embedded in executables.
        compare_versions: bool = false,
        /// Let a matching content result clear a quick-test difference.
        override_quick_test_results: bool = true,
        /// Content larger than this is compared as bytes rather than as text.
        binary_size_threshold_bytes: u64 = documented::BINARY_SIZE_THRESHOLD_BYTES,
    }
}

settings_group! {
    /// How the tree is scanned, expanded, refreshed and written.
    FolderHandlingSettings / FolderHandlingOverride {
        /// Read subfolders in the background so they can be colored before
        /// they are opened.
        scan_subfolders_in_background: bool = true,
        /// Scan top-level orphan folders as well, so their size is reported.
        scan_top_level_orphans: bool = false,
        /// Open every folder when the comparison loads.
        expand_subfolders_on_load: bool = false,
        /// Limit automatic expansion to folders containing differences.
        expand_only_folders_with_differences: bool = true,
        /// Set the local file's time to match a copy uploaded to a server that
        /// refuses client-set timestamps.
        touch_local_files_on_upload: bool = false,
        /// Present a link as its target, including type, size and time.
        follow_symbolic_links: bool = false,
        /// Preserve the short alias of a name when copying.
        maintain_short_name_aliases: bool = false,
        /// Copy security descriptors along with content.
        copy_file_permissions: bool = false,
        /// Preserve the original creation time when copying.
        copy_creation_dates: bool = false,
        /// Re-run the comparison periodically.
        automatic_refresh: bool = false,
        /// Minutes between automatic refreshes.
        automatic_refresh_minutes: u32 = provisional::AUTO_REFRESH_MINUTES,
        /// Whether archives are opened as folders or left as files.
        archive_handling: ArchiveHandling = ArchiveHandling::AsFolders,
    }
}

settings_group! {
    /// Masks deciding which names take part in the session.
    NameFilterSettings / NameFilterOverride {
        /// Masks for files to include, one per entry.
        include_files: Vec<String> = Vec::new(),
        /// Masks for files to exclude.
        exclude_files: Vec<String> = Vec::new(),
        /// Masks for folders to include.
        include_folders: Vec<String> = Vec::new(),
        /// Masks for folders to exclude.
        exclude_folders: Vec<String> = Vec::new(),
    }
}

settings_group! {
    /// Criteria excluding items for reasons other than their name.
    OtherFilterSettings / OtherFilterOverride {
        /// Exclusion criteria, all of which are applied.
        items: Vec<OtherFilterItem> = Vec::new(),
        /// Exclude files the operating system marks as protected.
        exclude_protected_system_files: bool = true,
    }
}

settings_group! {
    /// Per-session alignment rules and format enablement.
    FolderMiscSettings / FolderMiscOverride {
        /// Rules pairing differently named items.
        alignment_overrides: Vec<AlignmentOverrideItem> = Vec::new(),
        /// Formats forced on for this session regardless of the global state.
        enabled_formats: Vec<String> = Vec::new(),
        /// Formats forced off for this session regardless of the global state.
        disabled_formats: Vec<String> = Vec::new(),
    }
}

settings_group! {
    /// Direction and scope of a synchronization run.
    SyncSettings / SyncOverride {
        /// Direction and destructiveness of the run.
        method: SyncMethod = SyncMethod::UpdateRight,
        /// Copy items present on one side only.
        copy_orphans: bool = true,
        /// Delete items the method marks for removal rather than skipping them.
        allow_deletions: bool = true,
    }
}

settings_group! {
    /// Where a folder merge writes its result.
    MergeOutputSettings / MergeOutputOverride {
        /// Destination of the merged result.
        target: MergeTarget = MergeTarget::OutputFolder,
        /// Apply changes that resolve without a person deciding.
        automatic_merge: bool = true,
    }
}

settings_composite! {
    /// Settings a folder comparison session owns.
    FolderCompareSettings / FolderCompareOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Tests deciding whether a pair counts as changed.
        comparison: FolderComparisonSettings => FolderComparisonOverride,
        /// Scanning, expansion, refresh and copy behavior.
        handling: FolderHandlingSettings => FolderHandlingOverride,
        /// Name masks limiting the session's scope.
        name_filters: NameFilterSettings => NameFilterOverride,
        /// Non-name criteria limiting the session's scope.
        other_filters: OtherFilterSettings => OtherFilterOverride,
        /// Alignment rules and per-session format enablement.
        misc: FolderMiscSettings => FolderMiscOverride,
    }
}

settings_composite! {
    /// Settings a folder synchronization session owns.
    FolderSyncSettings / FolderSyncOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Tests deciding whether a pair counts as changed.
        comparison: FolderComparisonSettings => FolderComparisonOverride,
        /// Scanning, expansion, refresh and copy behavior.
        handling: FolderHandlingSettings => FolderHandlingOverride,
        /// Name masks limiting the session's scope.
        name_filters: NameFilterSettings => NameFilterOverride,
        /// Non-name criteria limiting the session's scope.
        other_filters: OtherFilterSettings => OtherFilterOverride,
        /// Direction and destructiveness of the run.
        sync: SyncSettings => SyncOverride,
    }
}

settings_composite! {
    /// Settings a folder merge session owns.
    FolderMergeSettings / FolderMergeOverride {
        /// Sides, ancestor, output and description.
        specs: SpecsSettings => SpecsOverride,
        /// Tests deciding whether a pair counts as changed.
        comparison: FolderComparisonSettings => FolderComparisonOverride,
        /// Scanning, expansion, refresh and copy behavior.
        handling: FolderHandlingSettings => FolderHandlingOverride,
        /// Name masks limiting the session's scope.
        name_filters: NameFilterSettings => NameFilterOverride,
        /// Non-name criteria limiting the session's scope.
        other_filters: OtherFilterSettings => OtherFilterOverride,
        /// Destination of the merged result.
        merge: MergeOutputSettings => MergeOutputOverride,
    }
}
