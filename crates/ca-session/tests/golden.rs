//! Golden-file tests: a stored document must survive a load and a save.
//!
//! The first fixture is the format this build writes, with every kind present.
//! The second is what a newer build could write: a higher schema version, a
//! kind, a node, enum variants and fields at every level this build has no
//! variant for. Neither may be quarantined, and neither may lose anything to a
//! save.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_session::kind::SessionKind;
use ca_session::share::{ImportOptions, SettingsPackage};
use ca_session::store::{SessionId, SessionStore, TreeNode, WorkspaceTab, SCHEMA_VERSION};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Copies a fixture into a scratch directory and returns where it landed, so a
/// test that saves never writes into the source tree.
fn scratch_copy(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::copy(fixture(name), &path).unwrap();
    path
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_current_format_re_saves_unchanged() {
    let dir = TempDir::new().unwrap();
    let path = scratch_copy(&dir, "v1-every-kind.json");
    let before = read_json(&path);
    let before_text = std::fs::read_to_string(&path).unwrap();

    let outcome = SessionStore::load(&path).unwrap();
    assert!(
        outcome.recovered_backup.is_none(),
        "the fixture is readable"
    );
    assert_eq!(outcome.migrated_from, None);
    assert_eq!(outcome.newer_schema, None);
    assert!(outcome.repairs.is_clean(), "{:?}", outcome.repairs);
    assert_eq!(outcome.store.schema_version, SCHEMA_VERSION);

    let mut store = outcome.store;
    store.save(&path).unwrap();
    assert_eq!(read_json(&path), before, "the document is unchanged");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before_text,
        "the canonical formatting is unchanged too"
    );
}

#[test]
fn the_current_format_carries_every_kind() {
    let outcome = SessionStore::load(&fixture("v1-every-kind.json")).unwrap();
    let kinds: Vec<SessionKind> = outcome
        .store
        .sessions()
        .iter()
        .map(|session| session.kind.clone())
        .collect();
    for kind in SessionKind::ALL {
        assert!(kinds.contains(kind), "{kind} is missing from the fixture");
    }
    for session in outcome.store.sessions() {
        session.resolve(&outcome.store.layers).unwrap();
    }
}

#[test]
fn a_document_from_a_newer_build_loads_without_quarantine() {
    let dir = TempDir::new().unwrap();
    let path = scratch_copy(&dir, "future-build.json");

    let outcome = SessionStore::load(&path).unwrap();
    assert!(
        outcome.recovered_backup.is_none(),
        "nothing a newer build wrote counts as damage"
    );
    assert_eq!(outcome.newer_schema, Some(2));
    assert!(outcome.store.is_newer_schema());

    let store = &outcome.store;
    assert_eq!(store.root.len(), 3);
    assert!(matches!(store.root[0].children()[1], TreeNode::Unknown(_)));
    assert_eq!(store.root[0].children()[1].name(), "Pinned");

    let chart = store.find_session(&SessionId::from_raw("n4")).unwrap();
    assert_eq!(
        chart.kind,
        SessionKind::Unknown("chart-compare".to_owned()),
        "a kind with no variant here is still a session"
    );
    assert!(chart.locked);

    let release = store.find_session(&SessionId::from_raw("n5")).unwrap();
    assert_eq!(release.kind, SessionKind::TextCompare);
    assert!(
        release.resolve(&store.layers).is_ok(),
        "a known session beside an unknown one still resolves"
    );

    let tabs = &store.workspaces[0].windows[0].tabs;
    assert_eq!(tabs.len(), 3);
    assert!(matches!(tabs[2], WorkspaceTab::Unknown(_)));
}

#[test]
fn a_document_from_a_newer_build_re_saves_without_losing_anything() {
    let dir = TempDir::new().unwrap();
    let path = scratch_copy(&dir, "future-build.json");
    let before = read_json(&path);

    let mut store = SessionStore::load(&path).unwrap().store;
    store.save(&path).unwrap();
    let after = read_json(&path);

    assert_eq!(
        after, before,
        "every unknown kind, node, variant and field is written back unchanged"
    );
    assert_eq!(
        after["schemaVersion"],
        Value::from(2),
        "a save never lowers the version a newer build wrote"
    );

    // A second pass proves the re-save is itself a fixed point.
    let mut store = SessionStore::load(&path).unwrap().store;
    store.save(&path).unwrap();
    assert_eq!(read_json(&path), before);
}

#[test]
fn a_newer_document_survives_editing_the_part_this_build_understands() {
    let dir = TempDir::new().unwrap();
    let path = scratch_copy(&dir, "future-build.json");
    let before = read_json(&path);

    let mut store = SessionStore::load(&path).unwrap().store;
    store
        .rename(&SessionId::from_raw("n5"), "Release notes")
        .unwrap();
    store.save(&path).unwrap();

    let after = read_json(&path);
    assert_eq!(after["root"][2]["name"], Value::from("Release notes"));
    assert_eq!(
        after["futureTopLevelField"], before["futureTopLevelField"],
        "an edit to a known session leaves the unknown parts alone"
    );
    assert_eq!(
        after["root"][0]["children"][1],
        before["root"][0]["children"][1]
    );
    assert_eq!(after["root"][1], before["root"][1]);
    assert_eq!(after["layers"], before["layers"]);
}

#[test]
fn a_package_from_a_newer_build_loads_and_re_saves_unchanged() {
    let dir = TempDir::new().unwrap();
    let path = scratch_copy(&dir, "future-package.json");
    let before = read_json(&path);

    let package = SettingsPackage::read(&path).unwrap();
    assert!(package.is_newer_schema());
    assert_eq!(package.sessions.len(), 2);
    assert_eq!(
        package.sessions[1].session.kind,
        SessionKind::Unknown("chart-compare".to_owned()),
        "a kind with no variant here is still a packaged session"
    );

    package.write(&path).unwrap();
    assert_eq!(
        read_json(&path),
        before,
        "every unknown field of the package is written back unchanged"
    );
}

#[test]
fn importing_a_newer_package_flags_the_version_and_counts_what_it_cannot_place() {
    let package = SettingsPackage::read(&fixture("future-package.json")).unwrap();
    let mut store = SessionStore::default();
    let report = package
        .import(&mut store, &ImportOptions::default())
        .unwrap();

    assert_eq!(report.newer_schema, Some(2));
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.sessions_added, 2);
    assert_eq!(report.workspaces_imported, 1);
    assert_eq!(
        report.unplaced_fields, 2,
        "the package field and the packaged-session field are counted, not dropped in silence"
    );

    let sessions = store.sessions();
    let chart = sessions
        .iter()
        .find(|session| session.name == "Chart")
        .unwrap();
    assert_eq!(chart.kind, SessionKind::Unknown("chart-compare".to_owned()));

    let nightly = sessions
        .iter()
        .find(|session| session.name == "Nightly")
        .unwrap();
    assert!(
        nightly.unknown.contains_key("futureSessionField"),
        "a field a newer build added inside a session travels with it"
    );

    let tabs = &store.workspace("Daily").unwrap().windows[0].tabs;
    assert_eq!(tabs.len(), 3);
    assert!(matches!(tabs[2], WorkspaceTab::Unknown(_)));
}
