//! One-off generator for the checked-in v1 fixture. Ignored by default.

#![allow(clippy::unwrap_used, missing_docs)]

use ca_session::kind::SessionKind;
use ca_session::location::SideLocation;
use ca_session::settings::folder::OtherFilterItem;
use ca_session::settings::{SessionSettings, SessionSettingsOverride};
use ca_session::store::{
    SavedSession, SessionStore, WindowBounds, Workspace, WorkspaceTab, WorkspaceWindow,
};

#[test]
#[ignore = "writes the checked-in fixture"]
fn generate() {
    let mut store = SessionStore::default();
    let folder = store.create_folder(None, "Team").unwrap();

    for kind in SessionKind::ALL {
        let id = store.next_id();
        let mut session = SavedSession::new(id, kind.title(), kind.clone());
        session.set_last_used_epoch_seconds(1_700_000_000);
        let mut full = SessionSettings::defaults_for(kind);
        if let SessionSettings::FolderCompare(settings) = &mut full {
            settings.specs.left = Some(SideLocation::local("/srv/left"));
            settings.specs.right = Some(SideLocation::archive("/srv/right.zip", "docs"));
            settings.comparison.timestamp_tolerance_seconds = 3;
            // Struct-variant fields are the spelling the defect class gets
            // wrong, so the fixture carries two of them.
            settings.other_filters.items = vec![
                OtherFilterItem::Modified {
                    older_than: true,
                    days_ago: Some(30),
                    absolute_seconds: None,
                    unknown: std::collections::BTreeMap::new(),
                },
                OtherFilterItem::UnixFileType {
                    is_not: true,
                    file_type: "symlink".to_owned(),
                    unknown: std::collections::BTreeMap::new(),
                },
            ];
        }
        let mut over = SessionSettingsOverride::from_full(&full);
        if !matches!(kind, SessionKind::FolderCompare) {
            over = SessionSettingsOverride::empty_for(kind);
        }
        session.settings = over;
        let parent = if kind.is_folder_kind() {
            Some(&folder)
        } else {
            None
        };
        store.add_session(parent, session).unwrap();
    }

    let locked = store.next_id();
    let mut session = SavedSession::new(locked.clone(), "Locked", SessionKind::TextCompare);
    session.locked = true;
    session.set_last_used_epoch_seconds(1_700_000_001);
    store.add_session(None, session).unwrap();

    let auto = store.next_id();
    let mut session = SavedSession::new(auto, "Untitled", SessionKind::HexCompare);
    session.set_last_used_epoch_seconds(1_700_000_002);
    store.record_auto_saved(session);

    store
        .layers
        .update_session_defaults_from(
            &SessionKind::TextCompare,
            &SessionSettings::defaults_for(&SessionKind::TextCompare),
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
                WorkspaceTab::saved(locked),
                WorkspaceTab::unsaved(SavedSession::new(
                    ca_session::store::SessionId::from_raw("u1"),
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

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("v1-every-kind.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    store.save_replacing(&path).unwrap();
}
