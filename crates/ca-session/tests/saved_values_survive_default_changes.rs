//! A value a user saved keeps its meaning when a default changes: only a field
//! absent from the document takes the current default.

#![allow(clippy::unwrap_used, missing_docs)]

use ca_session::options::ProgramOptions;
use ca_session::settings::binary::HexComparisonSettings;
use ca_session::settings::folder::{
    ContentComparison, FolderComparisonSettings, SyncMethod, SyncSettings,
};
use ca_session::settings::table::RowSettings;
use ca_session::settings::text::{TextAlignmentSettings, TextConflictSettings};

#[test]
fn saved_session_values_equal_to_a_former_default_load_unchanged() {
    let comparison: FolderComparisonSettings = serde_json::from_str(
        r#"{"timestampToleranceSeconds":0,"alignDifferentNormalization":false,"contentComparison":"none"}"#,
    )
    .unwrap();
    assert_eq!(comparison.timestamp_tolerance_seconds, 0);
    assert!(!comparison.align_different_normalization);
    assert_eq!(comparison.content_comparison, ContentComparison::None);

    let sync: SyncSettings = serde_json::from_str(r#"{"method":"update-right"}"#).unwrap();
    assert_eq!(sync.method, SyncMethod::UpdateRight);

    let conflicts: TextConflictSettings = serde_json::from_str(r#"{"separationLines":2}"#).unwrap();
    assert_eq!(conflicts.separation_lines, 2);

    let alignment: TextAlignmentSettings =
        serde_json::from_str(r#"{"skewTolerance":500}"#).unwrap();
    assert_eq!(alignment.skew_tolerance, 500);

    let rows: RowSettings = serde_json::from_str(r#"{"skewTolerance":500}"#).unwrap();
    assert_eq!(rows.skew_tolerance, 500);

    let hex: HexComparisonSettings =
        serde_json::from_str(r#"{"unlockedLoadLimitBytes":16777216}"#).unwrap();
    assert_eq!(hex.unlocked_load_limit_bytes, 16 * 1024 * 1024);
}

#[test]
fn a_session_field_absent_from_the_document_takes_the_current_default() {
    let comparison: FolderComparisonSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(comparison.timestamp_tolerance_seconds, 2);
    assert!(comparison.align_different_normalization);
    assert_eq!(comparison.content_comparison, ContentComparison::RulesBased);

    let sync: SyncSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(sync.method, SyncMethod::UpdateBoth);

    let conflicts: TextConflictSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(conflicts.separation_lines, 1);

    let alignment: TextAlignmentSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(alignment.skew_tolerance, 2000);

    let rows: RowSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(rows.skew_tolerance, 2000);

    let hex: HexComparisonSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(hex.unlocked_load_limit_bytes, 2 * 1024 * 1024);
}

#[test]
fn saved_option_values_equal_to_a_former_default_load_unchanged() {
    let document = r#"{
        "textEditing": {"contextLines": 2},
        "fileOperations": {"includeHiddenItems": true},
        "tweaks": {
            "dimInactivePanePercent": 0,
            "binaryBufferBytes": 65536,
            "hintDurationMilliseconds": 5000,
            "useOrphanColor": true,
            "automaticallyReloadUnlessChangesAreDiscarded": true,
            "escapeClosesFileViews": false
        }
    }"#;
    let options: ProgramOptions = serde_json::from_str(document).unwrap();
    assert_eq!(options.text_editing.context_lines, 2);
    assert!(options.file_operations.include_hidden_items);
    assert_eq!(options.tweaks.dim_inactive_pane_percent, 0);
    assert_eq!(options.tweaks.binary_buffer_bytes, 65_536);
    assert_eq!(options.tweaks.hint_duration_milliseconds, 5_000);
    assert!(options.tweaks.use_orphan_color);
    assert!(
        options
            .tweaks
            .automatically_reload_unless_changes_are_discarded
    );
    assert!(!options.tweaks.escape_closes_file_views);
}

#[test]
fn an_option_absent_from_the_document_takes_the_current_default() {
    let options: ProgramOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(options.text_editing.context_lines, 6);
    assert!(!options.file_operations.include_hidden_items);
    assert_eq!(options.tweaks.dim_inactive_pane_percent, 20);
    assert_eq!(options.tweaks.binary_buffer_bytes, 4_194_304);
    assert_eq!(options.tweaks.hint_duration_milliseconds, 2_500);
    assert!(!options.tweaks.use_orphan_color);
    assert!(
        !options
            .tweaks
            .automatically_reload_unless_changes_are_discarded
    );
    assert!(options.tweaks.escape_closes_file_views);
}
