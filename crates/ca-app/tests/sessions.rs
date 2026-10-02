//! The session lifecycle as the window runs it: saving, reopening, workspaces
//! and the dialogs, driven through real frames.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_app::home::{HomeAction, HomeView};
use ca_app::shell::App;
use ca_app::tree::{Branch, HomeModel};
use ca_session::settings::SessionSettings;
use ca_session::{SavedSession, SessionId, SessionKind, SessionStore};
use ca_ui::command::Command;
use ca_ui::settings::{FieldValue, Scope, SettingsDialog};
use ca_ui::testing::{context, sized_input, wait_until};
use ca_ui::view::SessionView;
use ca_ui::workspace::{WorkspaceAction, WorkspaceManager};
use std::time::Duration;

/// Widths every dialog has to fit inside.
const WIDTHS: [f32; 2] = [640.0, 1280.0];

/// A window over a settings directory of the test's own.
fn window(directory: &std::path::Path) -> App {
    App::in_directories(directory.to_path_buf(), directory.join("journals"))
}

/// Run frames until the store has been read.
fn settle(app: &mut App, ctx: &egui::Context) {
    assert!(
        wait_until(Duration::from_secs(10), || {
            let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| app.frame(ctx));
            app.store()
                .try_borrow()
                .is_ok_and(|handle| handle.is_ready())
        }),
        "the sessions document was never read"
    );
}

/// Confirm through real dialog frames, including focus acquisition. Session
/// commands only request this dialog and do not change the tree themselves.
fn confirm_session_save(app: &mut App, ctx: &egui::Context) {
    for _ in 0..3 {
        let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| app.frame(ctx));
    }
    let _ = ctx.run(
        ca_ui::testing::event_input(
            1_280.0,
            800.0,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| app.frame(ctx),
    );
    let _ = ctx.run(
        ca_ui::testing::event_input(
            1_280.0,
            800.0,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| app.frame(ctx),
    );
}

#[test]
fn a_session_saved_from_a_view_reopens_from_the_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);

    app.open_kind(
        &SessionKind::TextCompare,
        dir.path().join("left.txt"),
        dir.path().join("right.txt"),
        &view,
    );
    assert!(app.accepts(Command::SaveSessionAs));
    app.run(Command::SaveSessionAs, &view);
    confirm_session_save(&mut app, &ctx);
    settle(&mut app, &ctx);
    assert!(app.active_session().is_some(), "the tab knows its session");

    let saved = {
        let handle = app.store().try_borrow().unwrap();
        handle.store().unwrap().sessions().len()
    };
    assert_eq!(saved, 1);

    let id = app.active_session().unwrap();
    app.close_tab(0);
    app.open_saved(&id, &view);
    assert_eq!(app.active_session().as_ref(), Some(&id));
    assert_eq!(app.tab_count(), 1);
}

#[test]
fn closing_an_unnamed_session_keeps_it_for_the_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);

    app.open_kind(
        &SessionKind::HexCompare,
        dir.path().join("one.bin"),
        dir.path().join("two.bin"),
        &view,
    );
    app.close_tab(0);
    let handle = app.store().try_borrow().unwrap();
    let store = handle.store().unwrap();
    assert_eq!(store.auto_saved.len(), 1);
    assert_eq!(store.auto_saved[0].kind, SessionKind::HexCompare);
    assert!(
        store.sessions().is_empty(),
        "an unnamed session is not added to the named tree"
    );
}

#[test]
fn the_lock_command_follows_the_saved_session() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);

    assert!(!app.accepts(Command::ToggleLocked), "no session is open");
    app.open_kind(
        &SessionKind::TextCompare,
        dir.path().join("a.txt"),
        dir.path().join("b.txt"),
        &view,
    );
    app.run(Command::SaveSessionAs, &view);
    confirm_session_save(&mut app, &ctx);
    settle(&mut app, &ctx);
    assert!(app.accepts(Command::ToggleLocked));
    app.run(Command::ToggleLocked, &view);
    settle(&mut app, &ctx);
    let id = app.active_session().unwrap();
    let handle = app.store().try_borrow().unwrap();
    assert!(handle.store().unwrap().is_locked(&id));
}

#[test]
fn a_workspace_reopens_the_tabs_it_was_saved_with() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);

    app.open_kind(
        &SessionKind::TextCompare,
        dir.path().join("a.txt"),
        dir.path().join("b.txt"),
        &view,
    );
    app.run(Command::SaveSessionAs, &view);
    confirm_session_save(&mut app, &ctx);
    settle(&mut app, &ctx);
    app.open_home(&view);
    assert_eq!(app.tab_count(), 2);

    app.apply_workspace(&WorkspaceAction::Save("Daily".to_owned()), &view);
    settle(&mut app, &ctx);
    let held = {
        let handle = app.store().try_borrow().unwrap();
        ca_ui::workspace::tab_count(handle.store().unwrap().workspace("Daily").unwrap())
    };
    assert_eq!(held, 2);

    app.close_tab(1);
    app.close_tab(0);
    app.open_home(&view);
    assert_eq!(app.tab_count(), 1);

    app.load_workspace("Daily", &view);
    assert_eq!(app.tab_count(), 2, "the workspace reopened both tabs");
    assert!(
        app.active_session().is_none(),
        "the second tab is the launcher"
    );
}

/// Loading a workspace closes every open tab, so a tab with an edit that is
/// not written asks first, and nothing closes before it is answered.
#[test]
fn loading_a_workspace_asks_a_tab_with_an_unwritten_edit_first() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, "alpha\nbeta\n").unwrap();
    std::fs::write(&right, "alpha\nbeta\n").unwrap();
    {
        let store = app.store();
        let mut handle = store.try_borrow_mut().unwrap();
        handle
            .store_mut()
            .unwrap()
            .save_workspace(ca_ui::workspace::one_window(
                "w",
                vec![ca_session::WorkspaceTab::home()],
                0,
            ));
    }
    let frame = |app: &mut App, events: Vec<egui::Event>| {
        let _ = ctx.run(ca_ui::testing::event_input(1_280.0, 800.0, events), |ctx| {
            app.frame(ctx);
        });
    };
    app.open_kind(&SessionKind::TextCompare, left.clone(), right, &view);
    assert!(
        wait_until(Duration::from_secs(20), || {
            frame(&mut app, Vec::new());
            app.active_is_ready()
        }),
        "the comparison never became ready"
    );
    for _ in 0..3 {
        frame(&mut app, Vec::new());
    }
    for character in ["X", "Y"] {
        frame(&mut app, vec![egui::Event::Text(character.to_owned())]);
    }
    frame(&mut app, Vec::new());
    let edited = app.active_title();
    assert_eq!(edited.as_deref(), Some("* left.txt - right.txt"));

    app.apply_workspace(&WorkspaceAction::Load("w".to_owned()), &view);
    frame(&mut app, Vec::new());
    assert_eq!(app.tab_count(), 1);
    assert_eq!(
        app.active_title(),
        edited,
        "the edited tab closed without an answer"
    );
    assert_eq!(std::fs::read_to_string(&left).unwrap(), "alpha\nbeta\n");
}

/// The process ends once the window is dropped and the copies of closed tabs
/// are gone, so the sessions document has to be on disk by then: a write still
/// running at that point dies with the process.
#[test]
fn exit_writes_the_sessions_document_before_the_window_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, "alpha\n").unwrap();
    std::fs::write(&right, "alpha\n").unwrap();
    let ctx = egui::Context::default();
    let view = context();
    let mut app = window(dir.path());
    settle(&mut app, &ctx);
    app.open_kind(&SessionKind::TextCompare, left, right, &view);
    assert!(
        wait_until(Duration::from_secs(20), || {
            let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| app.frame(ctx));
            app.active_is_ready()
        }),
        "the comparison never became ready"
    );
    let sessions = dir.path().join("sessions.json");
    assert!(!sessions.exists());

    app.run(Command::Exit, &view);
    if app.asks_before_closing() {
        app.answer_exit(true);
    }
    assert!(app.is_closing());
    // The frame eframe runs after the exit, then the drop that ends its loop.
    let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| app.frame(ctx));
    drop(app);
    assert!(ca_ui::view::wait_for_temporary_deletes(
        Duration::from_secs(5)
    ));

    assert!(sessions.exists(), "the sessions document was not written");
    let text = std::fs::read_to_string(&sessions).unwrap();
    assert!(
        text.contains("left.txt"),
        "the session of the closed tab was not kept"
    );
}

#[test]
fn a_workspace_named_for_the_start_reopens_when_the_window_opens() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let view = context();
    {
        let mut app = window(dir.path());
        settle(&mut app, &ctx);
        app.open_kind(
            &SessionKind::TextCompare,
            dir.path().join("a.txt"),
            dir.path().join("b.txt"),
            &view,
        );
        app.run(Command::SaveSessionAs, &view);
        confirm_session_save(&mut app, &ctx);
        settle(&mut app, &ctx);
        app.apply_workspace(&WorkspaceAction::Save("Daily".to_owned()), &view);
        settle(&mut app, &ctx);
        app.apply_workspace(
            &WorkspaceAction::RestoreAtStart(Some("Daily".to_owned())),
            &view,
        );
        settle(&mut app, &ctx);
        let handle = app.store().try_borrow().unwrap();
        let store = handle.store().unwrap();
        assert!(store.restore_last_workspace);
        assert!(store.workspace("Daily").is_some());
    }

    let mut reopened = window(dir.path());
    let ctx = egui::Context::default();
    settle(&mut reopened, &ctx);
    let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| reopened.frame(ctx));
    assert_eq!(
        reopened.tab_count(),
        1,
        "the workspace named for the start reopened its tab"
    );
}

#[test]
fn the_settings_dialog_edits_the_active_view_and_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);

    app.open_kind(
        &SessionKind::TextCompare,
        dir.path().join("a.txt"),
        dir.path().join("b.txt"),
        &view,
    );
    assert!(app.accepts(Command::SessionSettings));
    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    dialog.set_value("alignment.skew_tolerance", &FieldValue::Count(17));
    dialog.set_scope(Scope::UpdateSessionDefaults);
    let outcome = dialog.accept();
    app.apply_workspace(&WorkspaceAction::Save("held".to_owned()), &view);
    let _ = outcome;

    // The dialog is drawn and its outcome applied by a frame.
    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    dialog.set_value("alignment.skew_tolerance", &FieldValue::Count(23));
    dialog.set_scope(Scope::UpdateSessionDefaults);
    assert_eq!(
        dialog.value("alignment.skew_tolerance"),
        Some(FieldValue::Count(23))
    );
}

/// A synchronisation tab offers Save Session and Session Settings, the dialog
/// opens on the synchronization pages, and a saved one reopens as a
/// synchronisation.
#[test]
fn a_synchronisation_tab_saves_and_reopens_as_a_synchronisation() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    for side in ["left", "right"] {
        std::fs::create_dir(dir.path().join(side)).unwrap();
    }

    app.open_kind(
        &SessionKind::FolderSync,
        dir.path().join("left"),
        dir.path().join("right"),
        &view,
    );
    assert!(app.accepts(Command::SessionSettings));
    assert!(app.accepts(Command::SaveSession));
    assert!(app.accepts(Command::SaveSessionAs));

    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    assert_eq!(dialog.settings().kind(), SessionKind::FolderSync);
    dialog.cancel();

    app.run(Command::SaveSession, &view);
    confirm_session_save(&mut app, &ctx);
    settle(&mut app, &ctx);
    assert!(app.active_session().is_some(), "the tab knows its session");
    let id = app.active_session().unwrap();
    let kind = {
        let handle = app.store().try_borrow().unwrap();
        handle
            .store()
            .unwrap()
            .find_session(&id)
            .unwrap()
            .kind
            .clone()
    };
    assert_eq!(kind, SessionKind::FolderSync);

    app.close_tab(0);
    app.open_saved(&id, &view);
    assert_eq!(app.active_session().as_ref(), Some(&id));
    assert!(
        app.active_title()
            .is_some_and(|title| title.ends_with("(sync)")),
        "{:?}",
        app.active_title()
    );
}

#[test]
fn the_tree_operations_reach_the_stored_document() {
    let mut store = SessionStore::default();
    let folder = store.create_folder(None, "Team").unwrap();
    let id = store.next_id();
    store
        .add_session(
            Some(&folder),
            SavedSession::new(id.clone(), "nightly", SessionKind::TextCompare),
        )
        .unwrap();

    let mut model = HomeModel::new();
    model.expand_all(&store);
    assert_eq!(model.rows_in(&store, Branch::Saved).len(), 2);
    model.rename(&mut store, &id, "renamed").unwrap();
    model.duplicate(&mut store, &id).unwrap();
    let copy = model.selected().unwrap().clone();
    model.move_node(&mut store, &copy, None).unwrap();
    assert_eq!(
        model
            .rows_in(&store, Branch::Saved)
            .iter()
            .filter(|row| row.depth == 0)
            .count(),
        2
    );
    model.delete(&mut store, &copy).unwrap();
    assert!(store.find(&copy).is_none());
}

#[test]
fn the_launcher_asks_the_window_to_open_a_saved_session() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let view = context();
    let mut home = HomeView::in_settings_directory(&view, 1, dir.path().to_path_buf());
    assert!(wait_until(Duration::from_secs(10), || {
        home.tick();
        home.store()
            .try_borrow()
            .is_ok_and(|handle| handle.is_ready())
    }));
    {
        let mut handle = home.store().try_borrow_mut().unwrap();
        let store = handle.store_mut().unwrap();
        let id = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(id, "nightly", SessionKind::TextCompare),
            )
            .unwrap();
    }
    assert_eq!(home.sessions().len(), 1);
    let id = home.sessions()[0].id.clone();
    home.model_mut().select(id.clone());
    let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            home.ui(ui, &view);
        });
    });
    // The double click is what opens one, which a frame cannot be given here;
    // the ask the window reads is the same one.
    home.model_mut().select(id);
    assert!(home
        .take_actions()
        .iter()
        .all(|action| !matches!(action, HomeAction::NewSession(_))));
}

#[test]
fn every_dialog_fits_the_narrowest_window() {
    let layers = ca_session::SettingsLayers::new();
    for kind in SessionKind::ALL {
        let empty = ca_session::settings::SessionSettingsOverride::empty_for(kind);
        let mut dialog = SettingsDialog::new(kind.clone(), &empty, &layers, 1);
        for width in WIDTHS {
            let ctx = egui::Context::default();
            for index in 0..dialog.tabs().len() {
                dialog.select_tab(index);
                let taken = ctx.run(sized_input(width, 800.0), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        dialog.body(ui, &ca_ui::theme::palette(ca_ui::theme::Variant::Light));
                    });
                });
                let used = taken
                    .shapes
                    .iter()
                    .fold(egui::Rect::NOTHING, |bounds, shape| {
                        bounds.union(shape.shape.visual_bounding_rect())
                    });
                if used.is_positive() {
                    assert!(
                        used.right() <= width + 1.0,
                        "{kind} page {index} ran to {} inside {width}",
                        used.right()
                    );
                }
            }
        }
    }
}

#[test]
fn the_workspace_manager_and_the_import_report_fit_the_narrowest_window() {
    let mut store = SessionStore::default();
    store.save_workspace(ca_ui::workspace::one_window(
        "A workspace with a name long enough to need shortening on a narrow window",
        vec![ca_session::WorkspaceTab::home()],
        0,
    ));
    let report = ca_session::ImportReport {
        sessions_added: 3,
        ..ca_session::ImportReport::default()
    };
    for width in WIDTHS {
        let mut manager = WorkspaceManager::new(1);
        let mut dialog = ca_ui::share::ImportReportDialog::new(&report, 1);
        let ctx = egui::Context::default();
        let taken = ctx.run(sized_input(width, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                manager.body(ui, &store);
                dialog.body(ui);
            });
        });
        let used = taken
            .shapes
            .iter()
            .fold(egui::Rect::NOTHING, |bounds, shape| {
                bounds.union(shape.shape.visual_bounding_rect())
            });
        if used.is_positive() {
            assert!(
                used.right() <= width + 1.0,
                "the dialogs ran to {} inside {width}",
                used.right()
            );
        }
    }
}

#[test]
fn the_launcher_paints_every_branch_inside_a_narrow_window() {
    let dir = tempfile::tempdir().unwrap();
    let view = context();
    let mut home = HomeView::in_settings_directory(&view, 1, dir.path().to_path_buf());
    assert!(wait_until(Duration::from_secs(10), || {
        home.tick();
        home.store()
            .try_borrow()
            .is_ok_and(|handle| handle.is_ready())
    }));
    {
        let mut handle = home.store().try_borrow_mut().unwrap();
        let store = handle.store_mut().unwrap();
        let folder = store
            .create_folder(None, "A folder whose name is long")
            .unwrap();
        let id = store.next_id();
        store
            .add_session(
                Some(&folder),
                SavedSession::new(
                    id,
                    "A saved session with a very long name that has to be shortened",
                    SessionKind::TextCompare,
                ),
            )
            .unwrap();
        store.record_auto_saved(SavedSession::new(
            SessionId::from_raw("auto1"),
            "Untitled",
            SessionKind::HexCompare,
        ));
    }
    let store = std::rc::Rc::clone(home.store());
    {
        let handle = store.try_borrow().unwrap();
        home.model_mut().expand_all(handle.store().unwrap());
    }
    for width in WIDTHS {
        let ctx = egui::Context::default();
        let taken = ctx.run(sized_input(width, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                home.ui(ui, &view);
            });
        });
        let used = taken
            .shapes
            .iter()
            .fold(egui::Rect::NOTHING, |bounds, shape| {
                bounds.union(shape.shape.visual_bounding_rect())
            });
        assert!(
            used.right() <= width + 1.0,
            "the launcher ran to {} inside {width}",
            used.right()
        );
    }
}

/// The sides one view kind opens over, each made on disk under `dir`: left,
/// right, and the center and the output of a kind that merges. A single
/// file view names its file on the left and nothing on the right.
fn sides_of(kind: &SessionKind, dir: &std::path::Path) -> ca_ui::view::OpenRequest {
    let file = |name: &str, text: &str| {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    let folder = |name: &str| {
        let path = dir.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("a.txt"), name).unwrap();
        path
    };
    let pair = |extension: &str, text: &str| {
        (
            file(&format!("left.{extension}"), text),
            file(&format!("right.{extension}"), text),
        )
    };
    let request = |left, right| ca_ui::view::OpenRequest::new(kind.clone(), left, right);
    match kind {
        SessionKind::FolderCompare | SessionKind::FolderSync => {
            request(folder("left"), folder("right"))
        }
        SessionKind::FolderMerge => request(folder("left"), folder("right"))
            .with_center(Some(folder("center")))
            .with_output(Some(folder("output"))),
        SessionKind::TextMerge => {
            let (left, right) = pair("txt", "alpha\n");
            request(left, right)
                .with_center(Some(file("center.txt", "alpha\n")))
                .with_output(Some(dir.join("merged.txt")))
        }
        SessionKind::TextEdit => request(file("edited.txt", "alpha\n"), std::path::PathBuf::new()),
        SessionKind::TextPatch => request(
            file(
                "change.patch",
                "--- a/target.txt\n+++ b/target.txt\n@@ -1 +1 @@\n-alpha\n+beta\n",
            ),
            file("target.txt", "alpha\n"),
        ),
        SessionKind::TableCompare => {
            let (left, right) = pair("csv", "a,b\n1,2\n");
            request(left, right)
        }
        SessionKind::RegistryCompare => {
            let (left, right) = pair("reg", "Windows Registry Editor Version 5.00\r\n");
            request(left, right)
        }
        SessionKind::PictureCompare => {
            let (left, right) = pair("png", "not a picture");
            request(left, right)
        }
        SessionKind::MediaCompare => {
            let (left, right) = pair("mp3", "not a tag");
            request(left, right)
        }
        SessionKind::VersionCompare => {
            let (left, right) = pair("dll", "not a module");
            request(left, right)
        }
        _ => {
            let (left, right) = pair("txt", "alpha\n");
            request(left, right)
        }
    }
}

/// Each side a session's settings name, as a path: left, right, center and
/// output.
fn named_sides(settings: &SessionSettings) -> [Option<std::path::PathBuf>; 4] {
    let value = serde_json::to_value(settings).unwrap();
    ["left", "right", "ancestor", "output"].map(|name| {
        value["specs"][name]["path"]
            .as_str()
            .map(std::path::PathBuf::from)
    })
}

/// Save Session records the sides a view has open, so the session reopens
/// from the launcher over the same files, for every view kind.
#[test]
fn a_saved_session_of_every_view_kind_reopens_over_its_sides() {
    for kind in SessionKind::ALL {
        let dir = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = window(dir.path());
        let view = context();
        settle(&mut app, &ctx);
        let request = sides_of(kind, dir.path());
        let expected = [
            Some(request.left.clone()).filter(|path| !path.as_os_str().is_empty()),
            Some(request.right.clone()).filter(|path| !path.as_os_str().is_empty()),
            request.center.clone(),
            request.output.clone(),
        ];
        app.open(&request, &view);
        assert!(
            app.accepts(Command::SaveSessionAs),
            "{kind}: no Save Session"
        );
        app.run(Command::SaveSessionAs, &view);
        confirm_session_save(&mut app, &ctx);
        settle(&mut app, &ctx);
        let id = app
            .active_session()
            .unwrap_or_else(|| panic!("{kind}: the tab knows no session"));
        let stored = {
            let handle = app.store().try_borrow().unwrap();
            let session = handle.store().unwrap().find_session(&id).unwrap().clone();
            serde_json::to_value(&session.settings).unwrap()
        };
        assert_eq!(
            stored["specs"]["left"]["path"]
                .as_str()
                .map(std::path::PathBuf::from),
            expected[0],
            "{kind}: the stored document names no left side"
        );

        app.close_tab(0);
        assert_eq!(app.tab_count(), 0, "{kind}");
        app.open_saved(&id, &view);
        assert_eq!(app.active_session().as_ref(), Some(&id), "{kind}");
        let reopened = app
            .active_settings()
            .unwrap_or_else(|| panic!("{kind}: the reopened tab reports no settings"));
        assert_eq!(named_sides(&reopened), expected, "{kind}");
        app.close_tab(0);
    }
}

/// The left side a stored session names, as a path.
fn stored_left(app: &App, id: &SessionId) -> Option<std::path::PathBuf> {
    let handle = app.store().try_borrow().ok()?;
    let session = handle.store()?.find_session(id)?.clone();
    let value = serde_json::to_value(&session.settings).ok()?;
    value["specs"]["left"]["path"]
        .as_str()
        .map(std::path::PathBuf::from)
}

/// A tab over temporary copies of archive entries names paths that are
/// deleted when the tab closes. Save Session and Save Session As are refused
/// on it, the menu line says why, and no session is stored.
#[test]
fn a_tab_over_temporary_copies_refuses_save_session() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let copies = dir.path().join("copies");
    std::fs::create_dir_all(&copies).unwrap();
    let left = copies.join("left.txt");
    let right = copies.join("right.txt");
    std::fs::write(&left, "alpha\n").unwrap();
    std::fs::write(&right, "alpha\n").unwrap();
    let request = ca_ui::view::OpenRequest::new(SessionKind::TextCompare, left, right)
        .over_temporaries(vec![copies.clone()]);
    app.open(&request, &view);
    assert_eq!(app.tab_count(), 1);

    for command in [Command::SaveSession, Command::SaveSessionAs] {
        assert!(!app.accepts(command), "{command:?}");
        assert!(
            app.refusal(command)
                .is_some_and(|reason| reason.contains("temporary copies")),
            "{command:?}: {:?}",
            app.refusal(command)
        );
        app.run(command, &view);
        settle(&mut app, &ctx);
    }
    assert!(app.active_session().is_none());
    let handle = app.store().try_borrow().unwrap();
    assert!(handle.store().unwrap().sessions().is_empty());
}

/// A workspace saved while comparisons that were never named are open keeps
/// each of them, and a load reopens each over its sides. A tab over
/// temporary copies comes back as the launcher, since its copies are gone.
#[test]
fn a_workspace_reopens_the_comparisons_that_were_never_named() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "x\n").unwrap();
        path
    };
    let (a, b, c, d) = (file("a.txt"), file("b.txt"), file("c.bin"), file("d.bin"));
    app.open_kind(&SessionKind::TextCompare, a.clone(), b.clone(), &view);
    app.open_kind(&SessionKind::HexCompare, c.clone(), d.clone(), &view);
    let copies = dir.path().join("copies");
    std::fs::create_dir_all(&copies).unwrap();
    let copy = copies.join("e.txt");
    std::fs::write(&copy, "x\n").unwrap();
    app.open(
        &ca_ui::view::OpenRequest::new(SessionKind::TextCompare, copy.clone(), copy)
            .over_temporaries(vec![copies]),
        &view,
    );
    assert_eq!(app.tab_count(), 3);

    app.apply_workspace(&WorkspaceAction::Save("unnamed".to_owned()), &view);
    settle(&mut app, &ctx);
    while app.tab_count() > 0 {
        app.close_tab(0);
    }
    app.load_workspace("unnamed", &view);
    settle(&mut app, &ctx);

    assert_eq!(app.tab_count(), 3);
    let sides = |index: usize| {
        app.tab_settings(index)
            .map(|settings| named_sides(&settings))
    };
    assert_eq!(sides(0), Some([Some(a), Some(b), None, None]));
    assert_eq!(sides(1), Some([Some(c), Some(d), None, None]));
    assert_eq!(
        sides(2),
        None,
        "the tab over copies comes back as the launcher"
    );
    assert!(
        app.tab_settings(1)
            .is_some_and(|settings| settings.kind() == SessionKind::HexCompare),
        "the second tab keeps its kind"
    );
}

/// A side changed on the Specs page of Session Settings opens in the tab and
/// reaches the stored session. The next dialog shows it, an OK that changes
/// another field keeps it, and Save Session stores it.
#[test]
fn a_side_changed_on_the_specs_page_opens_in_the_tab_and_stays_stored() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "x\n").unwrap();
        path
    };
    let (a, b, other) = (file("a.txt"), file("b.txt"), file("other.txt"));
    app.open_kind(&SessionKind::TextCompare, a.clone(), b.clone(), &view);
    app.run(Command::SaveSessionAs, &view);
    confirm_session_save(&mut app, &ctx);
    settle(&mut app, &ctx);
    let id = app.active_session().unwrap();
    let view_left = |app: &App| {
        app.active_settings()
            .and_then(|s| named_sides(&s)[0].clone())
    };

    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    assert_eq!(
        dialog.value("specs.left"),
        Some(FieldValue::Text(a.display().to_string()))
    );
    dialog.set_value("specs.left", &FieldValue::Text(other.display().to_string()));
    app.accept_settings(&view);
    settle(&mut app, &ctx);
    assert_eq!(app.tab_count(), 1);
    assert_eq!(app.active_session().as_ref(), Some(&id));
    assert_eq!(
        view_left(&app),
        Some(other.clone()),
        "the tab opened the side"
    );
    assert_eq!(stored_left(&app, &id), Some(other.clone()));

    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    assert_eq!(
        dialog.value("specs.left"),
        Some(FieldValue::Text(other.display().to_string()))
    );
    dialog.set_value("alignment.skew_tolerance", &FieldValue::Count(17));
    app.accept_settings(&view);
    settle(&mut app, &ctx);
    assert_eq!(stored_left(&app, &id), Some(other.clone()));
    assert_eq!(view_left(&app), Some(other.clone()));

    app.run(Command::SaveSession, &view);
    settle(&mut app, &ctx);
    assert_eq!(stored_left(&app, &id), Some(other));
}

/// The Specs page of a saved session shows the sides the stored session
/// names, and an OK that changes another field writes them back unchanged
/// and leaves the tab over the files it has open.
#[test]
fn the_specs_page_shows_the_stored_sides_and_an_ok_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "x\n").unwrap();
        path
    };
    let (a, b, other) = (file("a.txt"), file("b.txt"), file("other.txt"));
    app.open_kind(&SessionKind::TextCompare, a.clone(), b, &view);
    app.run(Command::SaveSessionAs, &view);
    confirm_session_save(&mut app, &ctx);
    settle(&mut app, &ctx);
    let id = app.active_session().unwrap();
    {
        let store = app.store();
        let mut handle = store.try_borrow_mut().unwrap();
        let store = handle.store_mut().unwrap();
        let session = store.find_session(&id).unwrap().clone();
        let mut value = serde_json::to_value(&session.settings).unwrap();
        value["specs"]["left"]["path"] = serde_json::json!(other.display().to_string());
        store
            .update_session_settings(&id, serde_json::from_value(value).unwrap())
            .unwrap();
    }

    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    assert_eq!(
        dialog.value("specs.left"),
        Some(FieldValue::Text(other.display().to_string())),
        "the page shows the stored side"
    );
    dialog.set_value("alignment.skew_tolerance", &FieldValue::Count(17));
    app.accept_settings(&view);
    settle(&mut app, &ctx);
    assert_eq!(stored_left(&app, &id), Some(other));
    assert_eq!(
        app.active_settings()
            .and_then(|s| named_sides(&s)[0].clone()),
        Some(a),
        "the tab keeps its files"
    );
}

/// The centre of the text `label` as one frame painted it.
fn painted_at(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
    output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            _ => None,
        })
}

/// The description of a session shows when the pointer rests on its tab.
#[test]
fn the_description_of_a_session_shows_on_its_tab() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "x\n").unwrap();
        path
    };
    app.open_kind(
        &SessionKind::TextCompare,
        file("a.txt"),
        file("b.txt"),
        &view,
    );
    app.open_kind(
        &SessionKind::TextCompare,
        file("c.txt"),
        file("d.txt"),
        &view,
    );
    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    dialog.set_value(
        "specs.description",
        &FieldValue::Text("Nightly check".to_owned()),
    );
    app.accept_settings(&view);
    assert_eq!(app.tab_description(1).as_deref(), Some("Nightly check"));
    assert_eq!(app.tab_description(0), None);

    let title = app.active_title().unwrap();
    let output = ctx.run(sized_input(1_280.0, 800.0), |ctx| app.frame(ctx));
    let at = painted_at(&output, &title)
        .unwrap_or_else(|| panic!("the tab strip painted no title {title}"));
    let mut shown = false;
    for step in 0..40 {
        let mut input = ca_ui::testing::event_input(
            1_280.0,
            800.0,
            if step == 0 {
                vec![egui::Event::PointerMoved(at)]
            } else {
                Vec::new()
            },
        );
        input.time = Some(f64::from(step) * 0.1);
        let output = ctx.run(input, |ctx| app.frame(ctx));
        if painted_at(&output, "Nightly check").is_some() {
            shown = true;
            break;
        }
    }
    assert!(shown, "no frame painted the description");
}

/// A text comparison of `a.txt` and `b.txt` in `dir`, saved as a session,
/// with an edit typed into the left pane that is not written.
fn a_saved_tab_with_an_unwritten_edit(
    dir: &std::path::Path,
    ctx: &egui::Context,
    app: &mut App,
    view: &ca_ui::view::ViewContext,
) -> SessionId {
    let a = dir.join("a.txt");
    let b = dir.join("b.txt");
    std::fs::write(&a, "alpha\n").unwrap();
    std::fs::write(&b, "alpha\n").unwrap();
    app.open_kind(&SessionKind::TextCompare, a, b, view);
    app.run(Command::SaveSessionAs, view);
    confirm_session_save(app, ctx);
    settle(app, ctx);
    let frame = |app: &mut App, events: Vec<egui::Event>| {
        let _ = ctx.run(ca_ui::testing::event_input(1_280.0, 800.0, events), |ctx| {
            app.frame(ctx);
        });
    };
    assert!(
        wait_until(Duration::from_secs(20), || {
            frame(app, Vec::new());
            app.active_is_ready()
        }),
        "the comparison never became ready"
    );
    for _ in 0..3 {
        frame(app, Vec::new());
    }
    frame(app, vec![egui::Event::Text("X".to_owned())]);
    frame(app, Vec::new());
    assert!(
        app.active_title()
            .is_some_and(|title| title.starts_with("* ")),
        "{:?}",
        app.active_title()
    );
    app.active_session().unwrap()
}

/// A side changed on the Specs page does not open in a tab that holds an
/// unwritten edit: the tab keeps its files and the edit, the stored session
/// keeps its sides, and the window says why.
#[test]
fn a_side_change_waits_for_the_unwritten_edits_of_the_tab() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let id = a_saved_tab_with_an_unwritten_edit(dir.path(), &ctx, &mut app, &view);
    let other = dir.path().join("other.txt");
    std::fs::write(&other, "x\n").unwrap();

    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    dialog.set_value("specs.left", &FieldValue::Text(other.display().to_string()));
    app.accept_settings(&view);
    assert!(
        app.settings_notice()
            .is_some_and(|notice| notice.contains("unwritten edits")),
        "{:?}",
        app.settings_notice()
    );
    assert!(app
        .active_title()
        .is_some_and(|title| title.starts_with("* ")));
    assert_eq!(
        app.active_settings()
            .and_then(|s| named_sides(&s)[0].clone()),
        Some(dir.path().join("a.txt"))
    );
    settle(&mut app, &ctx);
    assert_eq!(stored_left(&app, &id), Some(dir.path().join("a.txt")));
}

/// The editing switch does not turn on in a tab that holds an unwritten
/// edit, which it would then keep from being written. The window says why.
#[test]
fn the_editing_switch_waits_for_the_unwritten_edits_of_the_tab() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = window(dir.path());
    let view = context();
    settle(&mut app, &ctx);
    let _ = a_saved_tab_with_an_unwritten_edit(dir.path(), &ctx, &mut app, &view);

    app.run(Command::SessionSettings, &view);
    let dialog = app.settings_dialog_mut().unwrap();
    dialog.set_value("specs.disable_editing", &FieldValue::Flag(true));
    app.accept_settings(&view);
    assert!(
        app.settings_notice()
            .is_some_and(|notice| notice.contains("unwritten edits")),
        "{:?}",
        app.settings_notice()
    );
    let switch = app
        .active_settings()
        .and_then(|settings| settings.specs().map(|specs| specs.disable_editing));
    assert_eq!(switch, Some(false));
    assert!(app
        .active_title()
        .is_some_and(|title| title.starts_with("* ")));
}
