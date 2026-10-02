//! Oversized settings inputs remain at their original paths and cannot be
//! replaced by the fallback document used after a failed load.

#![allow(clippy::unwrap_used)]

use ca_session::{ProgramOptions, SessionStore, MAX_DOCUMENT_BYTES};

const BUDGET: u64 = 64 * 1024 * 1024;

#[test]
fn oversized_sessions_are_refused_without_quarantine_or_fallback_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(BUDGET + 1).unwrap();
    drop(file);
    assert!(SessionStore::load(&path).is_err());
    assert!(SessionStore::default().save_merging(&path).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), BUDGET + 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn oversized_options_are_refused_without_quarantine_or_fallback_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("options.json");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(BUDGET + 1).unwrap();
    drop(file);
    assert!(ProgramOptions::load(&path).is_err());
    assert!(ProgramOptions::peek(&path).is_none());
    assert!(ProgramOptions::default().save_merging(&path).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), BUDGET + 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// A valid document exactly at the budget still loads with unknown fields,
/// while the next byte is refused without being moved aside.
#[test]
fn the_exact_document_budget_accepts_valid_settings_and_preserves_unknown_data() {
    use std::io::Write;
    assert_eq!(BUDGET, MAX_DOCUMENT_BYTES);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let prefix = br#"{"schemaVersion":1,"probe":{"future":true}}"#;
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    file.write_all(prefix).unwrap();
    let mut remaining = usize::try_from(BUDGET).unwrap() - prefix.len();
    let spaces = [b' '; 8192];
    while remaining > 0 {
        let count = remaining.min(spaces.len());
        file.write_all(&spaces[..count]).unwrap();
        remaining -= count;
    }
    file.flush().unwrap();
    drop(file);
    let sessions = SessionStore::load(&path).unwrap();
    assert_eq!(
        sessions.store.unknown["probe"],
        serde_json::json!({"future":true})
    );
    assert!(sessions.recovered_backup.is_none());
    let options = ProgramOptions::load(&path).unwrap();
    assert_eq!(
        options.options.unknown["probe"],
        serde_json::json!({"future":true})
    );
    assert!(options.recovered_backup.is_none());
    assert!(ProgramOptions::peek(&path).is_some());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b" ").unwrap();
    drop(file);
    assert!(SessionStore::load(&path).is_err());
    assert!(ProgramOptions::load(&path).is_err());
    assert!(ProgramOptions::peek(&path).is_none());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), BUDGET + 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// Saving too much data must not replace the last readable document.
#[test]
fn oversized_serialization_preserves_the_previous_sessions_and_options() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    let mut store = SessionStore::default();
    store.save_replacing(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    store.unknown.insert(
        "large".to_owned(),
        serde_json::Value::String("x".repeat(usize::try_from(BUDGET).unwrap())),
    );
    assert!(store.save(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    drop(store);
    let path = dir.path().join("options.json");
    let mut options = ProgramOptions::default();
    options.save_replacing(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    options.unknown.insert(
        "large".to_owned(),
        serde_json::Value::String("x".repeat(usize::try_from(BUDGET).unwrap())),
    );
    assert!(options.save(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}
