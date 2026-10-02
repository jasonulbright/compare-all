//! An unknown field survives a load and a save, wherever it sits.
//!
//! The hand-written golden fixtures only cover the objects somebody thought to
//! put a field on. This test builds a document that carries every session kind,
//! every side variant, every tagged choice and every list item, then injects a
//! probe key into every JSON object at every depth and asserts that every probe
//! comes back. A type that loses unknown fields fails here without anybody
//! having to add a fixture line for it.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_session::kind::SessionKind;
use ca_session::location::SideLocation;
use ca_session::settings::common::{EncodingChoice, FileFormatChoice, ReplacementItem, RuleSide};
use ca_session::settings::folder::OtherFilterItem;
use ca_session::settings::table::TablePairing;
use ca_session::settings::{SessionSettings, SessionSettingsOverride};
use ca_session::share::{ExportSelection, ImportOptions, SettingsPackage};
use ca_session::store::{
    SavedSession, SessionId, SessionStore, WindowBounds, Workspace, WorkspaceTab, WorkspaceWindow,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use tempfile::TempDir;

#[test]
fn direct_typed_session_deserialization_keeps_a_wide_unknown_integer() {
    let document = r#"{"schemaVersion":1,"future":18446744073709551616}"#;
    let direct = serde_json::from_str::<SessionStore>(document);
    let buffered =
        serde_json::from_value::<SessionStore>(serde_json::from_str::<Value>(document).unwrap());
    assert!(direct.is_ok(), "direct from_str failed: {direct:?}");
    assert!(
        buffered.is_err(),
        "from_value unexpectedly accepted the probe"
    );
}

/// Objects whose keys are data rather than field names. A probe key inserted
/// into one of these is a new map entry, not an unknown field, and its value
/// must satisfy the map's value type.
const MAP_PATHS: &[&str] = &["/layers/sessionDefaults"];

/// Every side variant this build can write.
fn side_variants() -> Vec<SideLocation> {
    vec![
        SideLocation::local("/srv/left"),
        SideLocation::archive("/srv/right.zip", "docs"),
        SideLocation::snapshot("/srv/yesterday.snapshot"),
        SideLocation::clipboard(),
        SideLocation::registry(r"HKEY_CURRENT_USER\Software"),
        SideLocation::profile("nightly", "out"),
        SideLocation::remote("sftp", "example.test/srv"),
    ]
}

fn other_filter_variants() -> Vec<OtherFilterItem> {
    vec![
        OtherFilterItem::Modified {
            older_than: true,
            days_ago: Some(30),
            absolute_seconds: None,
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::Size {
            smaller_than: true,
            bytes: 1024,
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::Content {
            not_containing: true,
            text: "draft".to_owned(),
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::Attribute {
            is_not_set: false,
            attribute: "H".to_owned(),
            unknown: BTreeMap::new(),
        },
        OtherFilterItem::UnixFileType {
            is_not: true,
            file_type: "symlink".to_owned(),
            unknown: BTreeMap::new(),
        },
    ]
}

fn pairing_variants() -> Vec<TablePairing> {
    vec![
        TablePairing::unaligned(),
        TablePairing::by_left_name(),
        TablePairing::by_right_name(),
        TablePairing::Custom {
            pairs: vec![(Some(0), Some(1)), (Some(1), None)],
            unknown: BTreeMap::new(),
        },
    ]
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

/// Builds a store holding one session of every kind with every field set.
fn populated_store() -> SessionStore {
    let mut store = SessionStore::default();
    let folder = store.create_folder(None, "Team").unwrap();

    for kind in SessionKind::ALL {
        let id = store.next_id();
        let mut session = SavedSession::new(id, kind.title(), kind.clone());
        session.set_last_used_epoch_seconds(1_700_000_000);
        session.settings = SessionSettingsOverride::from_full(&SessionSettings::defaults_for(kind));
        let parent = if kind.is_folder_kind() {
            Some(&folder)
        } else {
            None
        };
        store.add_session(parent, session).unwrap();

        store
            .layers
            .update_session_defaults_from(kind, &SessionSettings::defaults_for(kind))
            .unwrap();
    }

    let auto = store.next_id();
    let mut session = SavedSession::new(auto, "Untitled", SessionKind::HexCompare);
    session.settings = SessionSettingsOverride::from_full(&SessionSettings::defaults_for(
        &SessionKind::HexCompare,
    ));
    store.record_auto_saved(session);

    let tabbed = store.next_id();
    store
        .add_session(
            None,
            SavedSession::new(tabbed.clone(), "Tabbed", SessionKind::TextCompare),
        )
        .unwrap();
    store.save_workspace(Workspace {
        name: "Daily".to_owned(),
        windows: vec![WorkspaceWindow {
            bounds: Some(WindowBounds {
                monitor: Some("DISPLAY1".to_owned()),
                scale: Some(1.5),
                ..WindowBounds::new(10, 20, 1200, 800)
            }),
            tabs: vec![
                WorkspaceTab::home(),
                WorkspaceTab::saved(tabbed),
                WorkspaceTab::unsaved(SavedSession::new(
                    SessionId::from_raw("u1"),
                    "Scratch",
                    SessionKind::TextEdit,
                )),
            ],
            active_tab: 1,
            ..WorkspaceWindow::default()
        }],
        shortcut: Some("Ctrl+1".to_owned()),
        ..Workspace::default()
    });

    store
}

/// Fills the slots a default carries empty, so every variant of every tagged
/// choice appears somewhere in the document.
fn fill_variants(value: &mut Value, sides: &mut std::iter::Cycle<std::vec::IntoIter<Value>>) {
    match value {
        Value::Array(items) => {
            for item in items {
                fill_variants(item, sides);
            }
        }
        Value::Object(object) => {
            if object.contains_key("left") && object.contains_key("description") {
                for slot in ["left", "right", "ancestor", "output"] {
                    object.insert(slot.to_owned(), sides.next().unwrap());
                }
            }
            if object.contains_key("leftFormat") {
                object.insert(
                    "leftFormat".to_owned(),
                    to_value(&FileFormatChoice::Named {
                        name: "Text".to_owned(),
                        unknown: BTreeMap::new(),
                    }),
                );
                object.insert(
                    "rightFormat".to_owned(),
                    to_value(&FileFormatChoice::detected()),
                );
                object.insert(
                    "leftEncoding".to_owned(),
                    to_value(&EncodingChoice::Named {
                        name: "utf-8".to_owned(),
                        unknown: BTreeMap::new(),
                    }),
                );
                object.insert(
                    "rightEncoding".to_owned(),
                    to_value(&EncodingChoice::from_format()),
                );
            }
            if object.contains_key("excludeProtectedSystemFiles") {
                object.insert(
                    "items".to_owned(),
                    Value::Array(other_filter_variants().iter().map(to_value).collect()),
                );
            }
            if object.contains_key("pairing") {
                let index = object.len() % pairing_variants().len();
                object.insert("pairing".to_owned(), to_value(&pairing_variants()[index]));
            }
            for (_, child) in object.iter_mut() {
                fill_variants(child, sides);
            }
        }
        _ => {}
    }
}

/// Builds the populated document as JSON, with every tagged variant present.
fn populated_document(dir: &TempDir) -> (std::path::PathBuf, Value) {
    let mut store = populated_store();
    let path = dir.path().join("sessions.json");
    store.save_replacing(&path).unwrap();
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    let mut sides = side_variants()
        .iter()
        .map(to_value)
        .collect::<Vec<_>>()
        .into_iter()
        .cycle();
    fill_variants(&mut document, &mut sides);
    set_replacement_items(&mut document);

    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();
    let mut loaded = SessionStore::load(&path).unwrap().store;
    loaded.save(&path).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    (path, after)
}

/// A replacement group is the only group whose single field is a list, so it is
/// filled by name rather than by shape.
fn set_replacement_items(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                set_replacement_items(item);
            }
        }
        Value::Object(object) => {
            if let Some(Value::Object(group)) = object.get_mut("replacements") {
                {
                    group.insert(
                        "items".to_owned(),
                        Value::Array(vec![to_value(&ReplacementItem {
                            find: "colour".to_owned(),
                            replace_with: "color".to_owned(),
                            match_case: true,
                            whole_words_only: true,
                            regular_expression: false,
                            side: RuleSide::Left,
                            unknown: BTreeMap::new(),
                        })]),
                    );
                }
            }
            for (_, child) in object.iter_mut() {
                set_replacement_items(child);
            }
        }
        _ => {}
    }
}

/// One step of a path into a JSON document.
#[derive(Debug, Clone)]
enum Step {
    Key(String),
    Index(usize),
}

fn render(path: &[Step]) -> String {
    let mut text = String::new();
    for step in path {
        match step {
            Step::Key(key) => {
                text.push('/');
                text.push_str(key);
            }
            Step::Index(index) => {
                text.push('/');
                text.push_str(&index.to_string());
            }
        }
    }
    text
}

/// Inserts one probe key into every object, and returns where each one landed.
fn inject(value: &mut Value, path: &mut Vec<Step>, probes: &mut Vec<(Vec<Step>, String)>) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(Step::Index(index));
                inject(item, path, probes);
                path.pop();
            }
        }
        Value::Object(object) => {
            let here = render(path);
            if !MAP_PATHS.contains(&here.as_str()) {
                let key = format!("probeField{}", probes.len());
                object.insert(key.clone(), Value::from(probes.len()));
                probes.push((path.clone(), key));
            }
            let keys: Vec<String> = object.keys().cloned().collect();
            for key in keys {
                if key.starts_with("probeField") {
                    continue;
                }
                path.push(Step::Key(key.clone()));
                if let Some(child) = object.get_mut(&key) {
                    inject(child, path, probes);
                }
                path.pop();
            }
        }
        _ => {}
    }
}

/// Inserts an arbitrary precision integer into every object that preserves
/// unknown fields, recording the positions for the round-trip assertion.
fn inject_wide_integer(
    value: &mut Value,
    path: &mut Vec<Step>,
    probes: &mut Vec<(Vec<Step>, String)>,
) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(Step::Index(index));
                inject_wide_integer(item, path, probes);
                path.pop();
            }
        }
        Value::Object(object) => {
            let here = render(path);
            if !MAP_PATHS.contains(&here.as_str()) {
                let key = format!("wideIntegerProbe{}", probes.len());
                let Ok(number) = serde_json::from_str::<Value>("18446744073709551616") else {
                    panic!("arbitrary precision JSON number must parse");
                };
                object.insert(key.clone(), number);
                probes.push((path.clone(), key));
            }
            let keys: Vec<String> = object.keys().cloned().collect();
            for key in keys {
                if key.starts_with("wideIntegerProbe") {
                    continue;
                }
                path.push(Step::Key(key.clone()));
                if let Some(child) = object.get_mut(&key) {
                    inject_wide_integer(child, path, probes);
                }
                path.pop();
            }
        }
        _ => {}
    }
}

fn at<'a>(document: &'a Value, path: &[Step]) -> Option<&'a Value> {
    let mut current = document;
    for step in path {
        current = match step {
            Step::Key(key) => current.get(key)?,
            Step::Index(index) => current.get(index)?,
        };
    }
    Some(current)
}

fn check(document: &Value, probes: &[(Vec<Step>, String)], label: &str) {
    let mut lost = Vec::new();
    for (index, (path, key)) in probes.iter().enumerate() {
        let object = at(document, path);
        let survived = object
            .and_then(|value| value.get(key))
            .is_some_and(|value| value == &Value::from(index));
        if !survived {
            lost.push(format!("{}#{key}", render(path)));
        }
    }
    assert!(
        lost.is_empty(),
        "{label}: {} of {} unknown fields were dropped: {lost:#?}",
        lost.len(),
        probes.len()
    );
}

fn probe_count(document: &Value) -> usize {
    match document {
        Value::Object(object) => 1 + object.values().map(probe_count).sum::<usize>(),
        Value::Array(items) => items.iter().map(probe_count).sum(),
        _ => 0,
    }
}

#[test]
fn the_populated_document_holds_every_side_and_choice_variant() {
    let dir = TempDir::new().unwrap();
    let (_, document) = populated_document(&dir);
    let text = serde_json::to_string(&document).unwrap();

    for side in side_variants() {
        let tag = to_value(&side);
        let tag = tag.get("type").and_then(Value::as_str).unwrap_or("profile");
        assert!(text.contains(tag), "no side of type {tag} in the document");
    }
    for item in other_filter_variants() {
        let value = to_value(&item);
        let tag = value.get("kind").and_then(Value::as_str).unwrap();
        assert!(text.contains(tag), "no filter of kind {tag}");
    }
    assert!(text.contains("\"detected\"") && text.contains("\"from-format\""));
    assert!(
        probe_count(&document) > 200,
        "the document is meant to be large: {} objects",
        probe_count(&document)
    );
}

#[test]
fn an_unknown_field_on_any_object_of_a_stored_document_survives_a_round_trip() {
    let dir = TempDir::new().unwrap();
    let (path, mut document) = populated_document(&dir);

    let mut probes = Vec::new();
    inject(&mut document, &mut Vec::new(), &mut probes);
    assert!(probes.len() > 200, "only {} objects probed", probes.len());
    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let mut store = SessionStore::load(&path).unwrap().store;
    store.save(&path).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    check(&after, &probes, "stored document");
    assert_eq!(after, document, "the document is written back unchanged");
}

#[test]
fn wide_unknown_integers_survive_a_load_and_save_at_every_object_depth() {
    let dir = TempDir::new().unwrap();
    let (path, mut document) = populated_document(&dir);
    let mut probes = Vec::new();
    inject_wide_integer(&mut document, &mut Vec::new(), &mut probes);
    assert!(probes.len() > 200, "only {} objects probed", probes.len());
    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let load = SessionStore::load(&path).unwrap();
    assert!(
        load.recovered_backup.is_none(),
        "valid newer values quarantined"
    );
    assert_eq!(
        load.store.sessions().len(),
        populated_store().sessions().len()
    );
    let mut store = load.store;
    store.save(&path).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    let expected = serde_json::from_str::<Value>("18446744073709551616").unwrap();
    for (position, key) in &probes {
        let value = at(&after, position).and_then(|object| object.get(key));
        assert_eq!(
            value,
            Some(&expected),
            "wide value lost at {}#{key}",
            render(position)
        );
    }
    assert_eq!(after, document, "all wide unknown values remain exact");
}

#[test]
fn mistyped_known_session_fields_use_defaults_and_survive_a_save() {
    let dir = TempDir::new().unwrap();
    let (path, mut document) = populated_document(&dir);
    let future_value = Value::String("written by a newer build".to_owned());
    document["nextId"] = future_value.clone();
    document["maxAutoSaved"] = future_value.clone();
    document["autoSaved"][0]["lastUsedEpochSeconds"] = future_value.clone();
    document["workspaces"][0]["windows"][0]["activeTab"] = future_value.clone();
    let wide_integer = serde_json::from_str::<Value>("18446744073709551616").unwrap();
    document["futureWideInteger"] = wide_integer.clone();
    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let load = SessionStore::load(&path).unwrap();
    assert!(
        load.recovered_backup.is_none(),
        "one changed field quarantined all sessions"
    );
    assert_eq!(
        load.store.sessions().len(),
        populated_store().sessions().len()
    );
    assert_eq!(load.store.auto_saved.len(), 1);
    assert_eq!(
        load.store.max_auto_saved,
        SessionStore::default().max_auto_saved
    );
    assert_eq!(load.store.workspaces[0].windows[0].active_tab, 0);
    let mut store = load.store;
    store.save(&path).unwrap();

    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["nextId"], future_value);
    assert_eq!(written["maxAutoSaved"], future_value);
    assert_eq!(written["futureWideInteger"], wide_integer);
    assert_eq!(
        written["autoSaved"][0]["lastUsedEpochSeconds"],
        future_value
    );
    assert_eq!(
        written["workspaces"][0]["windows"][0]["activeTab"],
        future_value
    );
}

#[test]
fn a_mistyped_field_in_known_settings_reports_unreadable_settings_clearly() {
    fn replace_description(value: &mut Value) -> bool {
        match value {
            Value::Object(object) => {
                if object.get("kind") == Some(&Value::String("text-compare".to_owned()))
                    && object.get("specs").is_some()
                {
                    object.get_mut("specs").unwrap()["description"] = Value::from(42);
                    return true;
                }
                object.values_mut().any(replace_description)
            }
            Value::Array(items) => items.iter_mut().any(replace_description),
            _ => false,
        }
    }

    let dir = TempDir::new().unwrap();
    let (path, mut document) = populated_document(&dir);
    assert!(replace_description(&mut document), "no text settings found");
    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let loaded = SessionStore::load(&path).unwrap();
    assert!(loaded.recovered_backup.is_none());
    let sessions = loaded.store.sessions();
    let Some(session) = sessions
        .into_iter()
        .find(|session| session.kind == SessionKind::TextCompare)
    else {
        panic!("the text session remains in the tree");
    };
    let error = session.resolve(&loaded.store.layers).unwrap_err();
    assert!(
        matches!(
            error,
            ca_session::Error::UnreadableSettings {
                kind: SessionKind::TextCompare
            }
        ),
        "{error}"
    );
    assert!(
        !error
            .to_string()
            .contains("expected TextCompare, found TextCompare"),
        "{error}"
    );
}

#[test]
fn a_package_with_one_newer_session_field_keeps_its_other_sessions() {
    let dir = TempDir::new().unwrap();
    let (path, _) = populated_document(&dir);
    let store = SessionStore::load(&path).unwrap().store;
    let package = SettingsPackage::export(&store, &ExportSelection::everything());
    let package_path = dir.path().join("future-package.json");
    package.write(&package_path).unwrap();
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&package_path).unwrap()).unwrap();
    let session_count = document["sessions"].as_array().unwrap().len();
    let future_value = Value::String("written by a newer build".to_owned());
    document["sessions"][0]["session"]["lastUsedEpochSeconds"] = future_value.clone();
    document["workspaces"][0]["windows"][0]["activeTab"] = future_value.clone();
    std::fs::write(
        &package_path,
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();

    let loaded = SettingsPackage::read(&package_path).unwrap();
    assert_eq!(loaded.sessions.len(), session_count);
    loaded.write(&package_path).unwrap();
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&package_path).unwrap()).unwrap();
    assert_eq!(
        written["sessions"][0]["session"]["lastUsedEpochSeconds"],
        future_value
    );
    assert_eq!(
        written["workspaces"][0]["windows"][0]["activeTab"],
        future_value
    );
}

#[test]
fn an_unknown_field_on_any_object_of_a_settings_package_survives_a_round_trip() {
    let dir = TempDir::new().unwrap();
    let (path, _) = populated_document(&dir);
    let store = SessionStore::load(&path).unwrap().store;

    let package = SettingsPackage::export(&store, &ExportSelection::everything());
    let package_path = dir.path().join("package.json");
    package.write(&package_path).unwrap();

    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&package_path).unwrap()).unwrap();
    let mut probes = Vec::new();
    inject(&mut document, &mut Vec::new(), &mut probes);
    assert!(probes.len() > 200, "only {} objects probed", probes.len());
    std::fs::write(
        &package_path,
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();

    let read = SettingsPackage::read(&package_path).unwrap();
    read.write(&package_path).unwrap();
    let after: Value =
        serde_json::from_str(&std::fs::read_to_string(&package_path).unwrap()).unwrap();

    check(&after, &probes, "settings package");
    assert_eq!(after, document, "the package is written back unchanged");
}

#[test]
fn an_unknown_tree_node_survives_export_and_the_import_report_counts_it() {
    let dir = TempDir::new().unwrap();
    let (path, mut document) = populated_document(&dir);
    let raw_node = serde_json::json!({
        "node": "future-chart",
        "id": "future-node-1",
        "name": "Later",
        "payload": { "precision": 18_446_744_073_709_551_616_u128 }
    });
    document["root"]
        .as_array_mut()
        .unwrap()
        .push(raw_node.clone());
    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let store = SessionStore::load(&path).unwrap().store;
    let package = SettingsPackage::export(&store, &ExportSelection::everything());
    assert_eq!(package.unknown_tree_nodes.len(), 1);
    assert_eq!(
        package.unknown_tree_nodes[0].folder_path,
        Vec::<String>::new()
    );
    assert_eq!(package.unknown_tree_nodes[0].node, raw_node);

    let package_path = dir.path().join("package.json");
    package.write(&package_path).unwrap();
    let reloaded = SettingsPackage::read(&package_path).unwrap();
    assert_eq!(reloaded.unknown_tree_nodes, package.unknown_tree_nodes);
    let report = reloaded
        .import(&mut SessionStore::default(), &ImportOptions::default())
        .unwrap();
    assert_eq!(report.unknown_tree_nodes_not_imported, 1);
}

/// Guards the file layout the two tests above depend on.
#[test]
fn the_map_paths_named_here_are_maps_in_the_document() {
    let dir = TempDir::new().unwrap();
    let (_, document) = populated_document(&dir);
    for map_path in MAP_PATHS {
        let mut current = &document;
        for segment in map_path.trim_start_matches('/').split('/') {
            current = current
                .get(segment)
                .unwrap_or_else(|| panic!("{map_path} is not in the document"));
        }
        assert!(
            current.as_object().is_some_and(Map::is_empty).eq(&false),
            "{map_path} is empty, so excluding it hides nothing"
        );
    }
}
