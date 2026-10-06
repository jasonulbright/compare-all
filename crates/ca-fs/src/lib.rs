//! Folder comparison engine: scanning, filtering, comparison criteria, name
//! alignment with status rollup, and display filter predicates.
//!
//! A session runs in four steps:
//!
//! 1. [`scan_with`] reads both sides, cancellably, collecting per-entry errors.
//! 2. [`align_trees`] lines the two trees up by name into one tree of pairs.
//! 3. [`compare_quick`] applies the listing-only tests, then
//!    [`compare_contents_parallel`] reads file contents on the rayon pool and
//!    streams results.
//! 4. [`display::visible`] decides what the view shows.

#![allow(clippy::module_name_repetitions)]
#![allow(clippy::struct_excessive_bools)]

pub mod cancel;
pub mod compare;
pub mod criteria;
pub mod display;
pub mod filter;
pub mod journal;
pub mod merge;
pub mod ops;
pub mod rules;
pub mod scan;
pub mod source;
pub mod sync;
pub mod zone;

use std::path::Path;

pub use cancel::Cancel;
pub use compare::{
    align_trees, apply_filters, compare_contents_parallel, compare_quick,
    compare_source_contents_parallel, folder_status, normalization_key, rollup, AlignmentOptions,
    AlignmentOverride, ContentFailure, ContentUpdate, Node, NodeStatus, PairFacts, StatusFlags,
};
pub use criteria::{
    binary_equal, compare_contents, compare_source_contents, compare_source_contents_with, crc32,
    file_version, is_cloud_placeholder, quick_compare, quick_compare_with, timestamps_match,
    AttributeComparison, CompareOptions, ContentError, ContentMethod, ContentOutcome, ContentSide,
    ContentTests, FileVersion, QuickDifference, QuickResult, QuickTests, RulesComparer, Side,
};
pub use display::{DisplayFilter, FolderDisplayFilter};
pub use filter::{
    parse_size, wildcard_match, AttributeKind, CaseSensitivity, ContentFilter, FilterContext,
    FilterTime, Mask, NameFilters, OtherFilter, OtherFilters, UnixFileType,
};
pub use journal::{
    read_records, recover, recover_all, retire, sweep, Journal, JournalRecord, JournalWriter,
    Recovery, KEEP_TROUBLED_DAYS,
};
pub use ops::exec::{
    execute, AbortOnError, ConflictDecision, ContinueOnError, Decision, Drift, ErrorPolicy,
    ExecutionContext, ExecutionReport, Journaling, Progress, StepOutcome, StepResult,
};
pub use ops::fsops::{AttributeChange, FileOps, ItemIdentity, RealFs, SyncWrite, TargetState};
pub use ops::vfsops::{write_refusal, Mount, SourceOps};

pub use merge::{
    compare3, compare3_sources, leaves_for_person, left_for_person, output_copies_an_input,
    output_holds_conflict_markers, output_written_after_inputs, plan_merge, Change,
    FolderMergeOptions, MergeBases, MergeCounts, MergeFilters, MergeInputs, MergeRefused,
    MergeRequest, MergeRow, MergeSources, MergeStatus, MergeTree, Pane, Resolution,
};
pub use ops::plan::{
    exclude_masks, plan_attributes, plan_copy, plan_delete, plan_exchange, plan_move,
    plan_new_folder, plan_rename, plan_to_folder, plan_touch, refuse_unsupported_targets,
    resolve_selection, resolve_selection_with, BackupOptions, Bases, Conflict, ExcludeMasks,
    OperationKind, OperationOptions, OperationPlan, PathOption, PathState, PlanSide, PlanSkip,
    PlanStep, RenameAction, RenameError, Sides, StepAction, StepExpectation, TouchSpec, Verify,
};
pub use rules::RulesEngine;
pub use scan::{
    scan, scan_source, scan_with, Attributes, Entry, EntryError, LinkKind, ScanError, ScanOptions,
    ScanProgress, ScanResult,
};
pub use source::{
    ArchiveFormat, ArchiveHandling, ArchiveTypes, EntryFacts, Limits, Source, SourceError,
    SourceKind, TimeFidelity,
};
pub use sync::{
    plan_sync, preview, PreviewRow, StatusKey, SyncAction, SyncPreset, SyncPreview, SyncRefused,
    SyncRules,
};
pub use zone::{local_offset_seconds, probed_offset};

/// Hash the full contents of a file with BLAKE3.
///
/// # Errors
/// Propagates the I/O error when the file cannot be opened or mapped.
pub fn hash_file(path: &Path) -> std::io::Result<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_mmap_rayon(path)?;
    Ok(hasher.finalize())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{hash_file, scan};

    #[test]
    fn hash_is_stable_for_equal_contents() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("l.bin");
        let right = dir.path().join("r.bin");
        std::fs::write(&left, b"payload").unwrap();
        std::fs::write(&right, b"payload").unwrap();
        assert_eq!(hash_file(&left).unwrap(), hash_file(&right).unwrap());
    }

    #[test]
    fn scan_re_export_is_usable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        assert_eq!(scan(dir.path()).unwrap().len(), 1);
    }
}
