//! A journal line uses one spelling for every field.
//!
//! An enum-level `rename_all` renames variants only. The named fields of a
//! struct variant need `rename_all_fields`, or they keep their Rust spelling
//! while the rest of the line follows the document's convention. A journal is
//! `snake_case` throughout, including the record tag.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_fs::ops::exec::StepOutcome;
use ca_fs::ops::plan::{OperationKind, StepAction};
use serde_json::Value;
use std::path::PathBuf;

fn lines() -> Vec<ca_fs::journal::JournalRecord> {
    use ca_fs::journal::JournalRecord;
    vec![
        JournalRecord::BatchStart {
            kind: OperationKind::Copy,
            steps: 2,
            unix_seconds: 1_758_153_600,
        },
        JournalRecord::StepBegin {
            index: 0,
            action: StepAction::CopyFile {
                source: PathBuf::from("/left/a"),
                target: PathBuf::from("/right/a"),
            },
            temporary: Some(PathBuf::from("/right/a.ca-part")),
            backup: Some(PathBuf::from("/right/a.bak")),
        },
        JournalRecord::StepEnd {
            index: 0,
            outcome: StepOutcome::CopiedSourceRemains {
                source: PathBuf::from("/left/a"),
                target: PathBuf::from("/right/a"),
                message: "in use".to_owned(),
            },
        },
        JournalRecord::BatchEnd {
            completed: 1,
            cancelled: false,
            aborted: true,
        },
    ]
}

fn collect_keys(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                out.push((path.to_owned(), key.clone()));
                collect_keys(child, &format!("{path}.{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_keys(child, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_journal_record_writes_snake_case_field_names() {
    for record in lines() {
        let written = serde_json::to_value(&record).unwrap();
        let mut keys = Vec::new();
        collect_keys(&written, "", &mut keys);
        assert!(!keys.is_empty());
        for (path, key) in keys {
            assert_eq!(
                key,
                key.to_lowercase(),
                "a journal line writes `{key}` at `{path}`; the spelling is snake_case"
            );
        }
    }
}

#[test]
fn every_journal_record_reads_back_as_written() {
    for record in lines() {
        let text = serde_json::to_string(&record).unwrap();
        let back: ca_fs::journal::JournalRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(back, record);
    }
}
