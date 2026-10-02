//! The stored documents use one spelling for every field.
//!
//! An enum-level `rename_all` renames variants only. Without
//! `rename_all_fields`, the named fields of a struct variant keep their Rust
//! spelling while every other field in the document is camelCase. Where the
//! type also carries a flattened unknown map, a key written in the documented
//! spelling lands in that map and the setting is dropped, so these tests check
//! the emitted spelling and the unknown maps together.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_session::location::SideLocation;
use ca_session::settings::common::{EncodingChoice, FileFormatChoice};
use ca_session::settings::folder::OtherFilterItem;
use ca_session::settings::table::TablePairing;
use ca_session::store::{SavedSession, SessionId, TreeNode, WorkspaceTab};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Every key of `value`, at every depth, with the path that reaches it.
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

fn assert_camel_case_keys(value: &Value, what: &str) {
    let mut keys = Vec::new();
    collect_keys(value, "", &mut keys);
    assert!(!keys.is_empty(), "{what} emits no keys");
    for (path, key) in keys {
        assert!(
            !key.contains('_'),
            "{what} writes `{key}` at `{path}`; the stored spelling is camelCase"
        );
    }
}

/// Serializes `value`, checks the spelling of every key it writes, and reads it
/// back. A field the writer spells one way and the reader expects another shows
/// up as an inequality here even when the key spelling passes.
fn round_trip<T>(value: &T, what: &str) -> Value
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let written = serde_json::to_value(value).unwrap();
    assert_camel_case_keys(&written, what);
    let back: T = serde_json::from_value(written.clone()).unwrap();
    assert_eq!(&back, value, "{what} does not survive a round trip");
    written
}

#[test]
fn the_written_document_uses_one_spelling_throughout() {
    let text = std::fs::read_to_string(fixture("v1-every-kind.json")).unwrap();
    let document: Value = serde_json::from_str(&text).unwrap();
    assert_camel_case_keys(&document, "sessions.json");
}

#[test]
fn every_other_filter_variant_writes_camel_case_and_keeps_its_unknown_map_empty() {
    let items = vec![
        OtherFilterItem::Modified {
            older_than: true,
            days_ago: Some(30),
            absolute_seconds: Some(-1),
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::Size {
            smaller_than: true,
            bytes: 1024,
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::Content {
            not_containing: true,
            text: "needle".to_owned(),
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::Attribute {
            is_not_set: true,
            attribute: "H".to_owned(),
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::UnixFileType {
            is_not: true,
            file_type: "symlink".to_owned(),
            unknown: BTreeMap::new(),
        },
    ];

    for item in &items {
        round_trip(item, "OtherFilterItem");
        assert!(
            matches!(item, OtherFilterItem::Unknown(_)) || unknown_of(item).is_empty(),
            "a documented field reached the unknown map"
        );
    }
}

fn unknown_of(item: &OtherFilterItem) -> BTreeMap<String, Value> {
    let written = serde_json::to_value(item).unwrap();
    let back: OtherFilterItem = serde_json::from_value(written).unwrap();
    match back {
        OtherFilterItem::Modified { unknown, .. }
        | OtherFilterItem::Size { unknown, .. }
        | OtherFilterItem::Content { unknown, .. }
        | OtherFilterItem::Attribute { unknown, .. }
        | OtherFilterItem::UnixFileType { unknown, .. } => unknown,
        OtherFilterItem::Unknown(_) => panic!("a documented item parsed as an unknown one"),
        _ => BTreeMap::new(),
    }
}

#[test]
fn an_earlier_spelling_of_a_renamed_field_still_loads() {
    let text = r#"{
        "kind": "modified",
        "older_than": true,
        "days_ago": 7,
        "absolute_seconds": null
    }"#;
    let item: OtherFilterItem = serde_json::from_str(text).unwrap();
    assert_eq!(
        item,
        OtherFilterItem::Modified {
            older_than: true,
            days_ago: Some(7),
            absolute_seconds: None,
            unknown: BTreeMap::new(),
        }
    );

    let text = r#"{ "kind": "unix-file-type", "is_not": true, "file_type": "fifo" }"#;
    let item: OtherFilterItem = serde_json::from_str(text).unwrap();
    assert_eq!(
        item,
        OtherFilterItem::UnixFileType {
            is_not: true,
            file_type: "fifo".to_owned(),
            unknown: BTreeMap::new(),
        }
    );

    let text = r#"{ "kind": "size", "smaller_than": true, "bytes": 1024 }"#;
    let item: OtherFilterItem = serde_json::from_str(text).unwrap();
    assert_eq!(
        item,
        OtherFilterItem::Size {
            smaller_than: true,
            bytes: 1024,
            unknown: BTreeMap::new(),
        }
    );

    let text = r#"{ "kind": "content", "not_containing": true, "text": "draft" }"#;
    let item: OtherFilterItem = serde_json::from_str(text).unwrap();
    assert_eq!(
        item,
        OtherFilterItem::Content {
            not_containing: true,
            text: "draft".to_owned(),
            unknown: BTreeMap::new(),
        }
    );

    let text = r#"{ "kind": "attribute", "is_not_set": true, "attribute": "H" }"#;
    let item: OtherFilterItem = serde_json::from_str(text).unwrap();
    assert_eq!(
        item,
        OtherFilterItem::Attribute {
            is_not_set: true,
            attribute: "H".to_owned(),
            unknown: BTreeMap::new(),
        }
    );
}

#[test]
fn the_other_struct_variant_enums_write_camel_case_too() {
    round_trip(&SideLocation::local("/srv/left"), "SideLocation::Local");
    round_trip(
        &SideLocation::archive("/srv/a.zip", "docs"),
        "SideLocation::Archive",
    );
    round_trip(
        &SideLocation::snapshot("/srv/s.cass"),
        "SideLocation::Snapshot",
    );
    round_trip(
        &SideLocation::registry("HKCU\\Software"),
        "SideLocation::Registry",
    );
    round_trip(
        &SideLocation::remote("sftp", "host/path"),
        "SideLocation::Remote",
    );
    round_trip(
        &SideLocation::profile("prod", "logs"),
        "SideLocation::Profile",
    );

    round_trip(
        &FileFormatChoice::Named {
            name: "C++".to_owned(),
            unknown: BTreeMap::new(),
        },
        "FileFormatChoice::Named",
    );
    round_trip(
        &EncodingChoice::Named {
            name: "utf-8".to_owned(),
            unknown: BTreeMap::new(),
        },
        "EncodingChoice::Named",
    );
    round_trip(
        &TablePairing::Custom {
            pairs: vec![(Some(0), None), (None, Some(1))],
            unknown: BTreeMap::new(),
        },
        "TablePairing::Custom",
    );

    let session = SavedSession::new(
        SessionId::from_raw("n1"),
        "Scratch",
        ca_session::kind::SessionKind::TextCompare,
    );
    round_trip(
        &TreeNode::Folder {
            id: SessionId::from_raw("n2"),
            name: "Team".to_owned(),
            children: vec![TreeNode::Session(session.clone())],
            unknown: BTreeMap::new(),
        },
        "TreeNode::Folder",
    );
    round_trip(
        &WorkspaceTab::saved(SessionId::from_raw("n1")),
        "WorkspaceTab::Saved",
    );
    round_trip(&WorkspaceTab::unsaved(session), "WorkspaceTab::Unsaved");
}
