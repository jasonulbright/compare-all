//! Which tabs and which fields the settings of one session kind carry.
//!
//! The tables here are the single description of the dialog. The dialog draws
//! them, the tests walk them, and a field reaches both from one line.

use super::field::{
    alignment_overrides_field, choice_field, column_choice, column_decimal, column_flag,
    column_seconds, columns_field, count_field, declare_encoding, declare_format, element_flag,
    flag_field, lines_field, other_filters_field, pairing_field, replacements_field, side_field,
    signed_field, text_field, Choice, Field, FieldShape, FieldValue,
};
use ca_session::settings::{SessionSettings, SessionSettingsOverride};
use ca_session::SessionKind;

/// One page of the settings dialog.
#[derive(Debug, Clone, Copy)]
pub struct Tab {
    /// Name on the tab strip.
    pub name: &'static str,
    /// Fields on the page, in the order they are drawn.
    pub fields: &'static [Field],
}

/// Algorithms pairing lines or rows.
const ALIGNMENT: &[Choice] = &[
    Choice {
        id: "unaligned",
        label: "Unaligned",
    },
    Choice {
        id: "standard",
        label: "Standard alignment",
    },
    Choice {
        id: "myers-ond",
        label: "Myers O(ND) alignment",
    },
    Choice {
        id: "patience",
        label: "Patience Diff alignment",
    },
];

/// Rules pairing bytes.
const BYTE_ALIGNMENT: &[Choice] = &[
    Choice {
        id: "complete",
        label: "Complete",
    },
    Choice {
        id: "fast",
        label: "Fast",
    },
    Choice {
        id: "none",
        label: "None",
    },
];

/// Encodings the character area of a byte pane offers.
const CHAR_ENCODING: &[Choice] = &[
    Choice {
        id: "ascii",
        label: "ASCII",
    },
    Choice {
        id: "ansi",
        label: "ANSI",
    },
];

/// Renderings the difference pane of an image comparison offers.
const PICTURE_MODE: &[Choice] = &[
    Choice {
        id: "tolerance",
        label: "Tolerance",
    },
    Choice {
        id: "mismatch-range",
        label: "Mismatch range",
    },
    Choice {
        id: "blend",
        label: "Blend",
    },
    Choice {
        id: "single-side",
        label: "Single side",
    },
    Choice {
        id: "channel-difference",
        label: "Channel difference",
    },
    Choice {
        id: "channel-xor",
        label: "Channel exclusive or",
    },
];

/// Which image a single-sided view renders.
const PICTURE_SINGLE_SIDE: &[Choice] = &[
    Choice {
        id: "left",
        label: "Left image",
    },
    Choice {
        id: "right",
        label: "Right image",
    },
];

/// How a table column's values are read.
const COLUMN_TYPE: &[Choice] = &[
    Choice {
        id: "general",
        label: "General",
    },
    Choice {
        id: "text",
        label: "Text",
    },
    Choice {
        id: "numeric",
        label: "Numeric",
    },
    Choice {
        id: "date",
        label: "Date",
    },
];

/// Content tests a folder comparison can run.
const CONTENT: &[Choice] = &[
    Choice {
        id: "none",
        label: "Do not read contents",
    },
    Choice {
        id: "crc",
        label: "CRC comparison",
    },
    Choice {
        id: "binary",
        label: "Binary comparison",
    },
    Choice {
        id: "rules-based",
        label: "Rules-based comparison",
    },
];

/// Direction and destructiveness of a synchronization run.
const SYNC_METHOD: &[Choice] = &[
    Choice {
        id: "update-left",
        label: "Update left",
    },
    Choice {
        id: "update-right",
        label: "Update right",
    },
    Choice {
        id: "update-both",
        label: "Update both",
    },
    Choice {
        id: "mirror-to-left",
        label: "Mirror to left",
    },
    Choice {
        id: "mirror-to-right",
        label: "Mirror to right",
    },
];

/// Where a folder merge writes its result.
const MERGE_TARGET: &[Choice] = &[
    Choice {
        id: "left",
        label: "Left folder",
    },
    Choice {
        id: "right",
        label: "Right folder",
    },
    Choice {
        id: "output-folder",
        label: "Output folder",
    },
];

/// Greatest tolerance a seconds field offers, one week.
const SECONDS_LIMIT: u64 = 7 * 24 * 60 * 60;
/// Greatest number of lines or rows a skew field offers.
const SKEW_LIMIT: u64 = 100_000;
/// Greatest size a byte threshold offers.
const SIZE_LIMIT: u64 = 1 << 40;

/// Declares the two sides, the editing guard and the description of a kind.
/// The second form draws the editing guard disabled with `reason`, for a kind
/// whose view does not read it.
macro_rules! two_sided_specs {
    ($variant:ident) => {
        &[
            side_field!($variant, left, "Left"),
            side_field!($variant, right, "Right"),
            flag_field!($variant, specs.disable_editing, "Disable editing"),
            text_field!($variant, specs.description, "Description"),
        ]
    };
    ($variant:ident, $reason:expr) => {
        &[
            side_field!($variant, left, "Left"),
            side_field!($variant, right, "Right"),
            flag_field!($variant, specs.disable_editing, "Disable editing").unavailable($reason),
            text_field!($variant, specs.description, "Description"),
        ]
    };
}

/// Declares the sides of a kind that reads an ancestor and writes an output.
/// The second form draws the editing guard disabled with `reason`.
macro_rules! merge_specs {
    ($variant:ident) => {
        &[
            side_field!($variant, left, "Left"),
            side_field!($variant, ancestor, "Center"),
            side_field!($variant, right, "Right"),
            side_field!($variant, output, "Merge to"),
            flag_field!($variant, specs.disable_editing, "Disable editing"),
            text_field!($variant, specs.description, "Description"),
        ]
    };
    ($variant:ident, $reason:expr) => {
        &[
            side_field!($variant, left, "Left"),
            side_field!($variant, ancestor, "Center"),
            side_field!($variant, right, "Right"),
            side_field!($variant, output, "Merge to"),
            flag_field!($variant, specs.disable_editing, "Disable editing").unavailable($reason),
            text_field!($variant, specs.description, "Description"),
        ]
    };
}

/// Why the editing guard of a folder kind takes no edit.
const FOLDER_EDITS: &str = "The folder operations, and the file views opened from this view, \
                            do not read this switch.";

/// Why the editing guard of a kind that changes no file takes no edit.
const NO_EDITS: &str = "This view changes no file, so the switch has nothing to turn off.";

/// Declares the format and encoding page of a kind that reads text.
macro_rules! format_fields {
    ($variant:ident) => {
        &[
            declare_format!($variant, left_format, "Left file format"),
            declare_format!($variant, right_format, "Right file format"),
            declare_encoding!($variant, left_encoding, "Left encoding"),
            declare_encoding!($variant, right_encoding, "Right encoding"),
        ]
    };
}

/// Declares the format page of a kind with no encoding of its own.
macro_rules! format_only_fields {
    ($variant:ident) => {
        &[
            declare_format!($variant, left_format, "Left file format").unavailable(NO_REGISTRY),
            declare_format!($variant, right_format, "Right file format").unavailable(NO_REGISTRY),
        ]
    };
}

/// Why a named file format cannot be chosen for a side that carries no text.
const NO_REGISTRY: &str =
    "The format is resolved from the file. Naming one waits on the format registry.";

/// Declares the tests deciding whether a pair of folder items counts as
/// changed. The `without_alignment` form leaves out the name alignment fields
/// for a kind whose engine does not read them.
macro_rules! folder_comparison {
    ($variant:ident) => {
        folder_comparison!(
            @fields $variant,
            [
                flag_field!(
                    $variant,
                    comparison.align_different_extensions,
                    "Align names with different extensions"
                ),
                flag_field!(
                    $variant,
                    comparison.align_different_normalization,
                    "Align names with different normalization forms"
                )
            ]
        )
    };
    ($variant:ident, without_alignment) => {
        folder_comparison!(@fields $variant, [])
    };
    (@fields $variant:ident, [$($alignment:expr),*]) => {
        &[
            flag_field!($variant, comparison.compare_size, "Compare file size"),
            flag_field!(
                $variant,
                comparison.compare_timestamps,
                "Compare timestamps"
            ),
            count_field!(
                $variant,
                comparison.timestamp_tolerance_seconds,
                u32,
                SECONDS_LIMIT,
                "Timestamp tolerance, seconds"
            ),
            flag_field!(
                $variant,
                comparison.ignore_daylight_saving,
                "Ignore daylight saving difference"
            ),
            flag_field!(
                $variant,
                comparison.ignore_timezone,
                "Ignore time zone differences"
            ),
            flag_field!(
                $variant,
                comparison.compare_filename_case,
                "Compare filename case"
            ),
            flag_field!(
                $variant,
                comparison.compare_attributes,
                "Compare file attributes"
            ),
            text_field!(
                $variant,
                comparison.compared_attributes,
                "Attributes compared"
            ),
            flag_field!(
                $variant,
                comparison.compare_permissions,
                "Compare permissions"
            ),
            flag_field!($variant, comparison.compare_owner, "Compare owner"),
            flag_field!($variant, comparison.compare_group, "Compare group"),
            $($alignment,)*
            choice_field!(
                $variant,
                comparison.content_comparison,
                CONTENT,
                "Compare contents"
            ),
            flag_field!(
                $variant,
                comparison.skip_content_if_quick_tests_match,
                "Skip if quick tests indicate the files are the same"
            ),
            flag_field!($variant, comparison.compare_versions, "Compare versions"),
            flag_field!(
                $variant,
                comparison.override_quick_test_results,
                "Override quick test results"
            ),
            count_field!(
                $variant,
                comparison.binary_size_threshold_bytes,
                u64,
                SIZE_LIMIT,
                "Read as bytes above, in bytes"
            ),
        ]
    };
}

/// Declares how a folder tree is scanned, expanded, refreshed and written.
macro_rules! folder_handling {
    ($variant:ident) => {
        folder_handling!(
            @fields $variant,
            choice_field!($variant, handling.archive_handling, ARCHIVES, "Archives")
        )
    };
    (@fields $variant:ident, $archives:expr) => {
        &[
            flag_field!(
                $variant,
                handling.scan_subfolders_in_background,
                "Scan subfolders in the background"
            ),
            flag_field!(
                $variant,
                handling.scan_top_level_orphans,
                "Scan top-level orphan subfolders"
            ),
            flag_field!(
                $variant,
                handling.expand_subfolders_on_load,
                "Expand subfolders when the session loads"
            ),
            flag_field!(
                $variant,
                handling.expand_only_folders_with_differences,
                "Expand only subfolders with differences"
            ),
            flag_field!(
                $variant,
                handling.touch_local_files_on_upload,
                "Touch local files when copying to a server"
            ),
            flag_field!(
                $variant,
                handling.follow_symbolic_links,
                "Follow symbolic links"
            ),
            flag_field!(
                $variant,
                handling.copy_file_permissions,
                "Copy file permissions"
            ),
            flag_field!(
                $variant,
                handling.copy_creation_dates,
                "Copy creation dates"
            ),
            flag_field!(
                $variant,
                handling.automatic_refresh,
                "Refresh automatically"
            ),
            count_field!(
                $variant,
                handling.automatic_refresh_minutes,
                u32,
                1_440,
                "Minutes between refreshes"
            ),
            $archives,
            flag_field!(
                $variant,
                handling.maintain_short_name_aliases,
                "Maintain short name aliases"
            )
            .unavailable(NO_SHORT_NAMES),
        ]
    };
}

/// How an archive appears in a folder tree.
const ARCHIVES: &[Choice] = &[
    Choice {
        id: "as-folders",
        label: "As folders",
    },
    Choice {
        id: "as-files",
        label: "As files",
    },
];

/// Why the short name alias of a copied item is not preserved.
const NO_SHORT_NAMES: &str = "Reading and writing a short name alias needs a platform call this \
                              build does not make.";

/// Declares the four name mask lists.
macro_rules! name_filters {
    ($variant:ident) => {
        &[
            lines_field!($variant, name_filters.include_files, "Include files"),
            lines_field!($variant, name_filters.exclude_files, "Exclude files"),
            lines_field!($variant, name_filters.include_folders, "Include folders"),
            lines_field!($variant, name_filters.exclude_folders, "Exclude folders"),
        ]
    };
}

/// Declares the non-name exclusions this build binds.
macro_rules! other_filters {
    ($variant:ident) => {
        &[
            flag_field!(
                $variant,
                other_filters.exclude_protected_system_files,
                "Exclude protected operating system files"
            ),
            other_filters_field!($variant),
        ]
    };
}

/// Declares which differences of a text comparison count.
macro_rules! text_importance {
    ($variant:ident) => {
        &[
            element_flag!(
                $variant,
                ca_session::settings::text::element::COMMENT,
                "comment",
                "Comments"
            ),
            element_flag!(
                $variant,
                ca_session::settings::text::element::STRING,
                "string",
                "Strings"
            ),
            element_flag!(
                $variant,
                ca_session::settings::text::element::NUMBER,
                "number",
                "Numbers"
            ),
            element_flag!(
                $variant,
                ca_session::settings::text::element::KEYWORD,
                "keyword",
                "Keywords"
            ),
            element_flag!(
                $variant,
                ca_session::settings::text::element::IDENTIFIER,
                "identifier",
                "Identifiers"
            ),
            flag_field!(
                $variant,
                importance.leading_whitespace_important,
                "Leading whitespace"
            ),
            flag_field!(
                $variant,
                importance.embedded_whitespace_important,
                "Embedded whitespace"
            ),
            flag_field!(
                $variant,
                importance.trailing_whitespace_important,
                "Trailing whitespace"
            ),
            flag_field!(
                $variant,
                importance.everything_else_important,
                "Everything else"
            ),
            flag_field!(
                $variant,
                importance.character_case_important,
                "Character case"
            ),
            flag_field!(
                $variant,
                importance.orphan_lines_always_important,
                "Orphan lines are always important"
            ),
            flag_field!(
                $variant,
                importance.compare_line_endings,
                "Compare line endings"
            ),
        ]
    };
}

/// Why a merge cannot be told to read a named grammar.
const MERGE_NO_GRAMMAR: &str =
    "A merge builds its regions from the line pass and reads no grammar, so a named format \
     changes no result.";

/// Why a merge ignores an importance entry.
const MERGE_NO_IMPORTANCE: &str =
    "A merge builds its regions from the line pass and classifies no elements, so this changes \
     no result.";

/// Declares the encoding page of a kind that reads text under no grammar.
///
/// The format pickers stay on the page, disabled: the page is the documented
/// one and a missing control reads as an oversight rather than a limit.
macro_rules! encoding_only_fields {
    ($variant:ident) => {
        &[
            declare_format!($variant, left_format, "Left file format")
                .unavailable(MERGE_NO_GRAMMAR),
            declare_format!($variant, right_format, "Right file format")
                .unavailable(MERGE_NO_GRAMMAR),
            declare_encoding!($variant, left_encoding, "Left encoding"),
            declare_encoding!($variant, right_encoding, "Right encoding"),
        ]
    };
}

/// Declares which differences of a merge count.
///
/// The whitespace, case, orphan and line ending entries reach the merge. The
/// grammar element entries stay on the page with the reason attached, because a
/// merge reads no grammar.
macro_rules! merge_importance {
    ($variant:ident) => {
        &[
            element_flag!(
                $variant,
                ca_session::settings::text::element::COMMENT,
                "comment",
                "Comments"
            )
            .unavailable(MERGE_NO_IMPORTANCE),
            element_flag!(
                $variant,
                ca_session::settings::text::element::STRING,
                "string",
                "Strings"
            )
            .unavailable(MERGE_NO_IMPORTANCE),
            element_flag!(
                $variant,
                ca_session::settings::text::element::NUMBER,
                "number",
                "Numbers"
            )
            .unavailable(MERGE_NO_IMPORTANCE),
            element_flag!(
                $variant,
                ca_session::settings::text::element::KEYWORD,
                "keyword",
                "Keywords"
            )
            .unavailable(MERGE_NO_IMPORTANCE),
            element_flag!(
                $variant,
                ca_session::settings::text::element::IDENTIFIER,
                "identifier",
                "Identifiers"
            )
            .unavailable(MERGE_NO_IMPORTANCE),
            flag_field!(
                $variant,
                importance.leading_whitespace_important,
                "Leading whitespace"
            ),
            flag_field!(
                $variant,
                importance.embedded_whitespace_important,
                "Embedded whitespace"
            ),
            flag_field!(
                $variant,
                importance.trailing_whitespace_important,
                "Trailing whitespace"
            ),
            flag_field!(
                $variant,
                importance.everything_else_important,
                "Everything else"
            ),
            flag_field!(
                $variant,
                importance.character_case_important,
                "Character case"
            ),
            flag_field!(
                $variant,
                importance.orphan_lines_always_important,
                "Orphan lines are always important"
            ),
            flag_field!(
                $variant,
                importance.compare_line_endings,
                "Compare line endings"
            ),
        ]
    };
}

/// Declares how lines are paired.
macro_rules! text_alignment {
    ($variant:ident) => {
        text_alignment!($variant,)
    };
    ($variant:ident, $($extra:expr),* $(,)?) => {
        &[
            choice_field!($variant, alignment.algorithm, ALIGNMENT, "Algorithm"),
            flag_field!(
                $variant,
                alignment.never_align_differences,
                "Never align differences"
            ),
            count_field!(
                $variant,
                alignment.skew_tolerance,
                u32,
                SKEW_LIMIT,
                "Skew tolerance, lines"
            ),
            flag_field!(
                $variant,
                alignment.use_closeness_matching,
                "Use closeness matching"
            ),
            $($extra),*
        ]
    };
}

/// The pages of a folder comparison.
const FOLDER_COMPARE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(FolderCompare, FOLDER_EDITS),
    },
    Tab {
        name: "Comparison",
        fields: folder_comparison!(FolderCompare),
    },
    Tab {
        name: "Handling",
        fields: folder_handling!(FolderCompare),
    },
    Tab {
        name: "Name Filters",
        fields: name_filters!(FolderCompare),
    },
    Tab {
        name: "Other Filters",
        fields: other_filters!(FolderCompare),
    },
    Tab {
        name: "Misc",
        fields: &[
            alignment_overrides_field!(FolderCompare),
            lines_field!(FolderCompare, misc.enabled_formats, "Formats enabled here"),
            lines_field!(
                FolderCompare,
                misc.disabled_formats,
                "Formats disabled here"
            ),
        ],
    },
];

/// Why a synchronization takes no orphan or deletion switch.
const SYNC_BY_METHOD: &str =
    "The method states which orphans a synchronization copies and which items it deletes.";

/// The pages of a folder synchronization.
const FOLDER_SYNC: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(FolderSync, FOLDER_EDITS),
    },
    Tab {
        name: "Sync",
        fields: &[
            choice_field!(FolderSync, sync.method, SYNC_METHOD, "Method"),
            flag_field!(FolderSync, sync.copy_orphans, "Copy orphans").unavailable(SYNC_BY_METHOD),
            flag_field!(FolderSync, sync.allow_deletions, "Allow deletions")
                .unavailable(SYNC_BY_METHOD),
        ],
    },
    Tab {
        name: "Comparison",
        fields: folder_comparison!(FolderSync),
    },
    Tab {
        name: "Handling",
        fields: folder_handling!(FolderSync),
    },
    Tab {
        name: "Name Filters",
        fields: name_filters!(FolderSync),
    },
    Tab {
        name: "Other Filters",
        fields: other_filters!(FolderSync),
    },
];

/// The pages of a folder merge.
const FOLDER_MERGE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: merge_specs!(FolderMerge, FOLDER_EDITS),
    },
    Tab {
        name: "Comparison",
        fields: folder_comparison!(FolderMerge, without_alignment),
    },
    Tab {
        name: "Handling",
        fields: folder_handling!(FolderMerge),
    },
    Tab {
        name: "Name Filters",
        fields: name_filters!(FolderMerge),
    },
    Tab {
        name: "Other Filters",
        fields: other_filters!(FolderMerge),
    },
    Tab {
        name: "Misc",
        fields: &[
            choice_field!(FolderMerge, merge.target, MERGE_TARGET, "Merge to"),
            flag_field!(FolderMerge, merge.automatic_merge, "Merge automatically"),
        ],
    },
];

/// The pages of a text comparison.
const TEXT_COMPARE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(TextCompare),
    },
    Tab {
        name: "Format",
        fields: format_fields!(TextCompare),
    },
    Tab {
        name: "Importance",
        fields: text_importance!(TextCompare),
    },
    Tab {
        name: "Alignment",
        fields: text_alignment!(TextCompare),
    },
    Tab {
        name: "Replacements",
        fields: &[replacements_field!(TextCompare)],
    },
];

/// The pages of a text merge, which has no replacement rules.
const TEXT_MERGE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: merge_specs!(TextMerge),
    },
    Tab {
        name: "Format",
        fields: encoding_only_fields!(TextMerge),
    },
    Tab {
        name: "Importance",
        fields: merge_importance!(TextMerge),
    },
    Tab {
        name: "Alignment",
        fields: text_alignment!(
            TextMerge,
            flag_field!(
                TextMerge,
                conflicts.same_lines_only,
                "Conflict on changed lines only"
            ),
            count_field!(
                TextMerge,
                conflicts.separation_lines,
                u32,
                SKEW_LIMIT,
                "Conflict separation, lines"
            ),
        ),
    },
];

/// Declares the format page of a kind that detects the format and the
/// encoding of every file it reads.
macro_rules! detected_format_fields {
    ($variant:ident) => {
        &[
            declare_format!($variant, left_format, "Left file format").unavailable(DETECTED),
            declare_format!($variant, right_format, "Right file format").unavailable(DETECTED),
            declare_encoding!($variant, left_encoding, "Left encoding").unavailable(DETECTED),
            declare_encoding!($variant, right_encoding, "Right encoding").unavailable(DETECTED),
        ]
    };
}

/// Why a single file view offers no format or encoding to choose.
const DETECTED: &str = "The view detects the format and the encoding of each file it reads.";

/// Why an editor offers no second side.
const ONE_FILE: &str = "The editor opens one file, the left side.";

/// The pages of a single-file editing session.
const TEXT_EDIT: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: &[
            side_field!(TextEdit, left, "Left"),
            side_field!(TextEdit, right, "Right").unavailable(ONE_FILE),
            flag_field!(TextEdit, specs.disable_editing, "Disable editing"),
            text_field!(TextEdit, specs.description, "Description"),
        ],
    },
    Tab {
        name: "Format",
        fields: detected_format_fields!(TextEdit),
    },
];

/// The pages of a patch viewing session.
const TEXT_PATCH: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(TextPatch),
    },
    Tab {
        name: "Format",
        fields: detected_format_fields!(TextPatch),
    },
];

/// The pages of a table comparison.
const TABLE_COMPARE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(TableCompare),
    },
    Tab {
        name: "Format",
        fields: format_fields!(TableCompare),
    },
    Tab {
        name: "Sheets",
        fields: &[
            pairing_field!(TableCompare, sheets, "Sheet pairing").unavailable(
                "The comparison reads one table per side. Sheet pairing waits on \
             multiple sheets being extracted.",
            ),
        ],
    },
    Tab {
        name: "Columns",
        fields: &[
            pairing_field!(TableCompare, columns, "Column pairing"),
            column_flag!(TableCompare, key, "Columns are keys"),
            column_flag!(TableCompare, use_default, "Use the session default"),
            column_choice!(TableCompare, column_type, COLUMN_TYPE, "Column type"),
            column_flag!(TableCompare, unimportant, "Columns are unimportant"),
            column_flag!(TableCompare, ignore_character_case, "Ignore character case"),
            column_flag!(TableCompare, ignore_whitespace, "Ignore whitespace"),
            column_decimal!(TableCompare, numeric_tolerance, "Numeric tolerance"),
            column_seconds!(
                TableCompare,
                date_tolerance_seconds,
                "Date tolerance, seconds"
            ),
            columns_field!(TableCompare),
        ],
    },
    Tab {
        name: "Rows",
        fields: &[
            choice_field!(TableCompare, rows.algorithm, ALIGNMENT, "Algorithm"),
            flag_field!(
                TableCompare,
                rows.never_align_differences,
                "Never align differences"
            ),
            count_field!(
                TableCompare,
                rows.skew_tolerance,
                u32,
                SKEW_LIMIT,
                "Skew tolerance, rows"
            ),
            flag_field!(
                TableCompare,
                rows.use_closeness_matching,
                "Use closeness matching"
            ),
            flag_field!(
                TableCompare,
                rows.sort_before_alignment,
                "Sort rows before alignment"
            ),
        ],
    },
];

/// The pages of a byte comparison.
const HEX_COMPARE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(HexCompare),
    },
    Tab {
        name: "Format",
        fields: format_only_fields!(HexCompare),
    },
    Tab {
        name: "Comparison",
        fields: &[
            choice_field!(
                HexCompare,
                comparison.alignment,
                BYTE_ALIGNMENT,
                "Alignment"
            ),
            count_field!(
                HexCompare,
                comparison.unlocked_load_limit_bytes,
                u64,
                SIZE_LIMIT,
                "Load without a lock up to, in bytes"
            )
            .unavailable("Every side is already read without holding a lock."),
            choice_field!(
                HexCompare,
                comparison.char_encoding,
                CHAR_ENCODING,
                "Character area encoding"
            ),
            count_field!(
                HexCompare,
                comparison.bytes_per_row,
                u32,
                1_024,
                "Bytes per row"
            ),
        ],
    },
];

/// The pages of an image comparison.
const PICTURE_COMPARE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(PictureCompare, NO_EDITS),
    },
    Tab {
        name: "Format",
        fields: format_only_fields!(PictureCompare),
    },
    Tab {
        name: "Comparison",
        fields: &[
            choice_field!(
                PictureCompare,
                comparison.display_mode,
                PICTURE_MODE,
                "Display mode"
            ),
            count_field!(PictureCompare, comparison.tolerance, u8, 255, "Tolerance"),
            flag_field!(
                PictureCompare,
                comparison.ignore_unimportant,
                "Ignore unimportant differences"
            ),
            count_field!(
                PictureCompare,
                comparison.blend_percent,
                u8,
                100,
                "Blend percent"
            ),
            choice_field!(
                PictureCompare,
                comparison.single_side,
                PICTURE_SINGLE_SIDE,
                "Single side shows"
            ),
            flag_field!(PictureCompare, comparison.ignore_alpha, "Ignore alpha"),
            flag_field!(
                PictureCompare,
                comparison.transparent_pixels_equal,
                "Transparent pixels are equal"
            ),
            flag_field!(PictureCompare, comparison.auto_scale, "Auto scale"),
            signed_field!(PictureCompare, comparison.offset_x, "Offset across"),
            signed_field!(PictureCompare, comparison.offset_y, "Offset down"),
        ],
    },
    Tab {
        name: "Replacements",
        fields: &[replacements_field!(PictureCompare)],
    },
];

/// The pages of a media comparison.
const MEDIA_COMPARE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(MediaCompare, NO_EDITS),
    },
    Tab {
        name: "Importance",
        fields: &[
            flag_field!(MediaCompare, importance.tags_important, "Tags"),
            flag_field!(MediaCompare, importance.stream_important, "Stream"),
            flag_field!(MediaCompare, importance.duration_important, "Duration"),
            count_field!(
                MediaCompare,
                importance.duration_tolerance_seconds,
                u32,
                SECONDS_LIMIT,
                "Duration tolerance, seconds"
            ),
            lines_field!(
                MediaCompare,
                importance.unimportant_tags,
                "Unimportant tags"
            ),
        ],
    },
];

/// The pages of a version resource comparison.
const VERSION_COMPARE: &[Tab] = &[
    Tab {
        name: "Specs",
        fields: two_sided_specs!(VersionCompare, NO_EDITS),
    },
    Tab {
        name: "Importance",
        fields: &[
            flag_field!(
                VersionCompare,
                importance.file_version_important,
                "File version"
            ),
            flag_field!(
                VersionCompare,
                importance.product_version_important,
                "Product version"
            ),
            flag_field!(
                VersionCompare,
                importance.string_fields_important,
                "String fields"
            ),
            lines_field!(
                VersionCompare,
                importance.unimportant_fields,
                "Unimportant fields"
            ),
        ],
    },
];

/// The single page of a registry comparison.
const REGISTRY_COMPARE: &[Tab] = &[Tab {
    name: "Specs",
    fields: two_sided_specs!(RegistryCompare),
}];

/// The pages the settings dialog draws for `kind`.
///
/// A kind this build has no settings for yields no page, which the dialog
/// reports rather than drawing an empty frame.
#[must_use]
pub fn tabs_for(kind: &SessionKind) -> &'static [Tab] {
    match kind {
        SessionKind::FolderCompare => FOLDER_COMPARE,
        SessionKind::FolderMerge => FOLDER_MERGE,
        SessionKind::FolderSync => FOLDER_SYNC,
        SessionKind::TextCompare => TEXT_COMPARE,
        SessionKind::TextMerge => TEXT_MERGE,
        SessionKind::TextEdit => TEXT_EDIT,
        SessionKind::TextPatch => TEXT_PATCH,
        SessionKind::TableCompare => TABLE_COMPARE,
        SessionKind::HexCompare => HEX_COMPARE,
        SessionKind::PictureCompare => PICTURE_COMPARE,
        SessionKind::MediaCompare => MEDIA_COMPARE,
        SessionKind::VersionCompare => VERSION_COMPARE,
        SessionKind::RegistryCompare => REGISTRY_COMPARE,
        _ => &[],
    }
}

/// Every field of a kind, in tab order.
#[must_use]
pub fn fields_for(kind: &SessionKind) -> Vec<&'static Field> {
    tabs_for(kind)
        .iter()
        .flat_map(|tab| tab.fields.iter())
        .collect()
}

/// Builds an override that sets only what `settings` states differently from
/// `defaults`.
///
/// A field left at the value the layer below supplies stays unset, so a later
/// change to the session defaults still reaches this session.
#[must_use]
pub fn override_against(
    settings: &SessionSettings,
    defaults: &SessionSettings,
) -> SessionSettingsOverride {
    let full = SessionSettingsOverride::from_full(settings);
    let (Ok(mut written), Ok(inherited)) = (
        serde_json::to_value(&full),
        serde_json::to_value(SessionSettingsOverride::from_full(defaults)),
    ) else {
        return full;
    };
    prune_equal(&mut written, &inherited);
    serde_json::from_value::<SessionSettingsOverride>(written).unwrap_or(full)
}

/// Drops from `written` every entry `inherited` already states the same way.
///
/// The kind tag is never dropped: it is what an override is matched against,
/// not a value the layer below could supply.
fn prune_equal(written: &mut serde_json::Value, inherited: &serde_json::Value) {
    let (serde_json::Value::Object(written), serde_json::Value::Object(inherited)) =
        (written, inherited)
    else {
        return;
    };
    written.retain(|key, value| {
        key == "kind" || inherited.get(key).is_none_or(|other| other != value)
    });
    for (key, value) in written.iter_mut() {
        if let Some(other) = inherited.get(key) {
            prune_equal(value, other);
        }
    }
}

/// Copies every field of `defaults` over `settings`, so the session inherits
/// again.
pub fn reset_to(settings: &mut SessionSettings, defaults: &SessionSettings) {
    let kind = settings.kind();
    for field in fields_for(&kind) {
        if let Some(value) = field.read(defaults) {
            field.write(settings, &value);
        }
    }
}

/// True when the value of `field` in `settings` is drawn from the layer below.
#[must_use]
pub fn is_inherited(field: &Field, settings: &SessionSettings, defaults: &SessionSettings) -> bool {
    !field.is_overridden(settings, defaults)
}

/// The shape and current value of a field, for a test or a summary line.
#[must_use]
pub fn value_of(field: &Field, settings: &SessionSettings) -> Option<(FieldShape, FieldValue)> {
    field.read(settings).map(|value| (field.shape, value))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{fields_for, override_against, reset_to, tabs_for, Field};
    use ca_session::settings::{SessionSettings, SessionSettingsOverride};
    use ca_session::{SessionKind, SettingsLayers};

    #[test]
    fn every_kind_this_build_ships_has_at_least_a_specs_page() {
        for kind in SessionKind::ALL {
            let tabs = tabs_for(kind);
            assert!(!tabs.is_empty(), "{kind} has no settings page");
            assert_eq!(tabs[0].name, "Specs", "{kind} does not open on Specs");
        }
    }

    #[test]
    fn an_unknown_kind_has_no_page() {
        assert!(tabs_for(&SessionKind::Unknown("chart-compare".into())).is_empty());
    }

    /// The text edit and text patch views detect the format and the encoding
    /// of each file, and the editor opens one file, so those fields are drawn
    /// disabled with a reason. The sides the views open stay editable.
    #[test]
    fn the_single_file_pages_disable_what_their_views_do_not_read() {
        let formats = [
            "format.left_format",
            "format.right_format",
            "format.left_encoding",
            "format.right_encoding",
        ];
        for (kind, disabled_right) in [
            (SessionKind::TextEdit, true),
            (SessionKind::TextPatch, false),
        ] {
            let fields = fields_for(&kind);
            let unavailable = |key: &str| {
                fields.iter().find(|field| field.key == key).map_or_else(
                    || panic!("{kind} does not declare {key}"),
                    |field| field.unavailable.is_some(),
                )
            };
            for key in formats {
                assert!(unavailable(key), "{kind} offers {key}");
            }
            assert!(!unavailable("specs.left"), "{kind}");
            assert_eq!(unavailable("specs.right"), disabled_right, "{kind}");
        }
    }

    /// The editing switch takes an edit on the pages of the kinds whose view
    /// refuses every edit and every write under it, and is drawn disabled
    /// with its reason on every other page. The description takes an edit on
    /// every page.
    #[test]
    fn the_editing_switch_is_live_only_where_a_view_reads_it() {
        let live = [
            SessionKind::TextCompare,
            SessionKind::TextEdit,
            SessionKind::TextPatch,
            SessionKind::TextMerge,
            SessionKind::TableCompare,
            SessionKind::HexCompare,
            SessionKind::RegistryCompare,
        ];
        for kind in SessionKind::ALL {
            let fields = fields_for(kind);
            let field = |key: &str| {
                fields
                    .iter()
                    .find(|field| field.key == key)
                    .unwrap_or_else(|| panic!("{kind} does not declare {key}"))
            };
            assert_eq!(
                field("specs.disable_editing").unavailable.is_none(),
                live.contains(kind),
                "{kind}"
            );
            assert!(field("specs.description").unavailable.is_none(), "{kind}");
        }
    }

    #[test]
    fn a_field_reads_only_the_kind_it_was_declared_for() {
        let field = fields_for(&SessionKind::HexCompare)
            .into_iter()
            .find(|field| field.key == "comparison.bytes_per_row")
            .unwrap();
        let hex = SessionSettings::defaults_for(&SessionKind::HexCompare);
        assert!(field.read(&hex).is_some());
        let text = SessionSettings::defaults_for(&SessionKind::TextCompare);
        assert!(field.read(&text).is_none());
    }

    #[test]
    fn every_field_key_is_unique_within_its_kind() {
        for kind in SessionKind::ALL {
            let mut keys: Vec<&str> = fields_for(kind).iter().map(|field| field.key).collect();
            let count = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), count, "{kind} declares a key twice");
        }
    }

    #[test]
    fn every_field_reads_and_writes_the_defaults_of_its_kind() {
        for kind in SessionKind::ALL {
            let defaults = SessionSettings::defaults_for(kind);
            for field in fields_for(kind) {
                let value = field
                    .read(&defaults)
                    .unwrap_or_else(|| panic!("{kind} {} cannot be read", field.key));
                let mut copy = defaults.clone();
                field.write(&mut copy, &value);
                assert_eq!(copy, defaults, "{kind} {} does not round trip", field.key);
            }
        }
    }

    #[test]
    fn a_field_left_alone_stays_unset_in_the_override() {
        let layers = SettingsLayers::new();
        let defaults = layers.resolve_defaults(&SessionKind::TextCompare);
        let overrides = override_against(&defaults, &defaults);
        assert!(
            overrides.is_empty(),
            "settings equal to the defaults still wrote an override"
        );
    }

    #[test]
    fn a_changed_field_reaches_the_override_and_nothing_else_does() {
        let layers = SettingsLayers::new();
        let defaults = layers.resolve_defaults(&SessionKind::TextCompare);
        let mut edited = defaults.clone();
        let field = fields_for(&SessionKind::TextCompare)
            .into_iter()
            .find(|field| field.key == "alignment.skew_tolerance")
            .unwrap();
        field.write(&mut edited, &crate::settings::field::FieldValue::Count(42));
        let overrides = override_against(&edited, &defaults);
        assert!(!overrides.is_empty());
        let SessionSettingsOverride::TextCompare(text) = &overrides else {
            panic!("kind changed");
        };
        assert_eq!(text.alignment.skew_tolerance, Some(42));
        assert_eq!(text.alignment.use_closeness_matching, None);
        assert_eq!(
            layers
                .resolve(&SessionKind::TextCompare, &overrides)
                .unwrap(),
            edited
        );
    }

    /// The importance page of a text comparison carries one entry per grammar
    /// element class, and each entry writes the class it names.
    #[test]
    fn the_importance_page_carries_the_element_checklist() {
        use ca_session::settings::text::element;
        let fields = fields_for(&SessionKind::TextCompare);
        for (key, name) in [
            ("importance.element.comment", element::COMMENT),
            ("importance.element.string", element::STRING),
            ("importance.element.number", element::NUMBER),
            ("importance.element.keyword", element::KEYWORD),
            ("importance.element.identifier", element::IDENTIFIER),
        ] {
            let field = fields
                .iter()
                .find(|field| field.key == key)
                .unwrap_or_else(|| panic!("{key} is not on any page"));
            let mut settings = SessionSettings::defaults_for(&SessionKind::TextCompare);
            field.write(
                &mut settings,
                &crate::settings::field::FieldValue::Flag(false),
            );
            let SessionSettings::TextCompare(text) = &settings else {
                panic!("kind changed");
            };
            assert!(!text.importance.element_important(name));
            for (_, other) in element::ALL.iter().enumerate().filter(|(_, o)| **o != name) {
                assert!(
                    text.importance.element_important(other),
                    "{key} also unchecked {other}"
                );
            }
        }
    }

    /// The merge engine reads no importance class, so the merge page offers
    /// only the entry it does read.
    /// The merge view draws no syntax coloring and its rules come from no
    /// grammar, so it offers no format picker.
    #[test]
    fn the_merge_format_page_offers_only_the_encodings() {
        let bound: Vec<&str> = page_of(&SessionKind::TextMerge, "Format")
            .iter()
            .filter(|field| field.is_bound())
            .map(|field| field.key)
            .collect();
        assert_eq!(bound, ["format.left_encoding", "format.right_encoding"]);
        assert!(
            page_of(&SessionKind::TextMerge, "Format")
                .iter()
                .any(|field| field.key == "format.left_format" && !field.is_bound()),
            "the format picker is drawn with its reason rather than dropped"
        );
    }

    #[test]
    fn the_merge_importance_page_offers_only_what_the_merge_engine_reads() {
        let page = page_of(&SessionKind::TextMerge, "Importance");
        let bound: Vec<&str> = page
            .iter()
            .filter(|field| field.is_bound())
            .map(|field| field.key)
            .collect();
        assert_eq!(
            bound,
            [
                "importance.leading_whitespace_important",
                "importance.embedded_whitespace_important",
                "importance.trailing_whitespace_important",
                "importance.everything_else_important",
                "importance.character_case_important",
                "importance.orphan_lines_always_important",
                "importance.compare_line_endings",
            ]
        );
        assert_eq!(
            page.len(),
            fields_of_page(&SessionKind::TextCompare, "Importance"),
            "the merge page carries the same entries as the comparison page"
        );
    }

    /// The fields of one named page of a kind.
    fn page_of(kind: &SessionKind, name: &str) -> &'static [Field] {
        tabs_for(kind)
            .iter()
            .find(|tab| tab.name == name)
            .map_or(&[][..], |tab| tab.fields)
    }

    /// How many fields one named page of a kind carries.
    fn fields_of_page(kind: &SessionKind, name: &str) -> usize {
        page_of(kind, name).len()
    }

    /// The conflict scope of a merge reaches the dialog, and no other kind
    /// carries it.
    #[test]
    fn the_conflict_scope_is_on_the_merge_alignment_page() {
        let merge: Vec<&str> = fields_for(&SessionKind::TextMerge)
            .iter()
            .map(|field| field.key)
            .collect();
        assert!(merge.contains(&"conflicts.same_lines_only"));
        assert!(merge.contains(&"conflicts.separation_lines"));
        let compare: Vec<&str> = fields_for(&SessionKind::TextCompare)
            .iter()
            .map(|field| field.key)
            .collect();
        assert!(!compare.iter().any(|key| key.starts_with("conflicts.")));
    }

    /// A folder page offers no control for a value this build cannot carry
    /// into the engines.
    #[test]
    fn the_folder_pages_offer_only_fields_this_build_acts_on() {
        for kind in [
            SessionKind::FolderCompare,
            SessionKind::FolderSync,
            SessionKind::FolderMerge,
        ] {
            let fields = fields_for(&kind);
            let field = |key: &str| {
                fields
                    .iter()
                    .find(|field| field.key == key)
                    .unwrap_or_else(|| panic!("{kind} does not declare {key}"))
            };
            assert!(
                field("handling.maintain_short_name_aliases")
                    .unavailable
                    .is_some(),
                "{kind} offers short name aliases as an editable control"
            );
            assert!(
                field("handling.archive_handling").unavailable.is_none(),
                "{kind} archive handling"
            );
            let keys: Vec<&str> = fields.iter().map(|field| field.key).collect();
            assert!(keys.contains(&"other_filters.items"), "{kind} filter list");
        }
        let compare: Vec<&str> = fields_for(&SessionKind::FolderCompare)
            .iter()
            .map(|field| field.key)
            .collect();
        assert!(compare.contains(&"misc.alignment_overrides"));
        let sync = fields_for(&SessionKind::FolderSync);
        let unavailable = |key: &str| {
            sync.iter()
                .find(|field| field.key == key)
                .map(|field| field.unavailable.is_some())
        };
        assert_eq!(unavailable("sync.method"), Some(false));
        assert_eq!(unavailable("sync.copy_orphans"), Some(true));
        assert_eq!(unavailable("sync.allow_deletions"), Some(true));
    }

    /// The three way folder comparison reads no name alignment option, so its
    /// page offers no name alignment field.
    #[test]
    fn the_folder_merge_comparison_page_omits_the_name_alignment_fields() {
        let alignment = [
            "comparison.align_different_extensions",
            "comparison.align_different_normalization",
        ];
        let merge: Vec<&str> = fields_for(&SessionKind::FolderMerge)
            .iter()
            .map(|field| field.key)
            .collect();
        let compare: Vec<&str> = fields_for(&SessionKind::FolderCompare)
            .iter()
            .map(|field| field.key)
            .collect();
        for key in alignment {
            assert!(!merge.contains(&key), "folder merge offers {key}");
            assert!(compare.contains(&key), "folder compare lost {key}");
        }
        assert!(merge.contains(&"comparison.content_comparison"));
    }

    #[test]
    fn a_reset_returns_every_field_to_the_layer_below() {
        let layers = SettingsLayers::new();
        let defaults = layers.resolve_defaults(&SessionKind::FolderCompare);
        let mut edited = defaults.clone();
        for field in fields_for(&SessionKind::FolderCompare) {
            if let Some(crate::settings::field::FieldValue::Flag(value)) = field.read(&edited) {
                field.write(
                    &mut edited,
                    &crate::settings::field::FieldValue::Flag(!value),
                );
            }
        }
        assert_ne!(edited, defaults);
        reset_to(&mut edited, &defaults);
        assert_eq!(edited, defaults);
    }
}
