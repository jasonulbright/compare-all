//! The snapshot payload uses one spelling for every field.
//!
//! The payload is `snake_case` throughout. A type
//! that mixes spellings drops settings in silence: both the document and each
//! record carry a flattened unknown map, so a key written in one spelling and
//! read in another lands in that map instead of the typed field.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_vfs::entry::{VfsAttributes, VfsLinkKind};
use ca_vfs::snapshot::{RecordedFidelity, Snapshot, SnapshotRecord};
use serde_json::Value;
use std::collections::BTreeMap;

fn populated() -> Snapshot {
    Snapshot {
        origin: "C:/projects/example".to_owned(),
        captured: Some(1_758_153_600),
        has_crc: true,
        entries: vec![SnapshotRecord {
            path: "src/main.rs".to_owned(),
            dir: false,
            size: 1024,
            modified: Some(1_758_067_200),
            time_fidelity: Some(RecordedFidelity::LocalTwoSecond),
            created: Some(1_758_067_200),
            attributes: Some(VfsAttributes {
                read_only: false,
                hidden: false,
                system: false,
                archive: true,
                windows_bits: Some(32),
                unix_mode: Some(0o644),
                uid: Some(1000),
                gid: Some(1000),
            }),
            crc32: Some(305_419_896),
            link: Some(VfsLinkKind::FileLink),
            version_info: Some("1.0.0".to_owned()),
            error: None,
            refused: true,
            unknown: BTreeMap::new(),
        }],
        unknown: BTreeMap::new(),
    }
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
fn the_payload_writes_snake_case_keys() {
    let written = serde_json::to_value(populated()).unwrap();
    let mut keys = Vec::new();
    collect_keys(&written, "", &mut keys);
    assert!(!keys.is_empty());
    for (path, key) in keys {
        assert_eq!(
            key,
            key.to_lowercase(),
            "the payload writes `{key}` at `{path}`; the stored spelling is snake_case"
        );
    }
}

#[test]
fn a_payload_using_the_documented_spelling_leaves_every_unknown_map_empty() {
    let value = populated();
    let text = serde_json::to_string(&value).unwrap();
    let back: Snapshot = serde_json::from_str(&text).unwrap();
    assert_eq!(back, value);
    assert!(back.unknown.is_empty(), "{:?}", back.unknown);
    for entry in &back.entries {
        assert!(entry.unknown.is_empty(), "{:?}", entry.unknown);
    }
}
