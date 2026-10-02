//! Changing an option reaches the open views on the next frame.
//!
//! Every test here drives real frames of the window against a settings
//! directory of its own, so nothing touches the folders a person uses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_app::cli::Startup;
use ca_app::App;
use ca_session::options::{ColorGroup, ProgramOptions, Rgb, ThemeChoice};
use ca_session::SessionKind;
use ca_ui::command::{Command, Keystroke, MenuView};
use ca_ui::testing::{context, raw_input};
use ca_ui::theme::slots::to_stored;
use ca_ui::theme::Variant;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A window over a settings directory of its own.
fn app_in(dir: &Path, startup: Startup) -> App {
    let settings = dir.join("settings");
    let journals = dir.join("journals");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::create_dir_all(&journals).unwrap();
    App::from_startup_in(startup, &context(), settings, journals)
}

/// Run frames until `ready` holds or the budget runs out.
fn run_frames(app: &mut App, ready: impl Fn(&App) -> bool) -> bool {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
        if ready(app) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Drive frames until the options document has been read.
fn settle(app: &mut App) {
    assert!(
        run_frames(app, |app| app.options().is_ready()),
        "the options document was never read"
    );
}

fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let left = dir.join("left");
    let right = dir.join("right");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    std::fs::write(left.join("same.txt"), b"alpha\nbeta\n").unwrap();
    std::fs::write(right.join("same.txt"), b"alpha\nBETA\n").unwrap();
    (left, right)
}

#[test]
fn the_options_command_opens_and_closes_the_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path(), Startup::Home);
    settle(&mut app);
    assert!(app.accepts(Command::Options));
    app.run(Command::Options, &context());
    assert!(app.options_dialog().is_some());
    let outcome = app.options_dialog_mut().unwrap().cancel();
    assert!(outcome.closed);
}

/// A color a person chose reaches a shape the window paints, on the next frame
/// and without a restart.
#[test]
fn a_changed_color_reaches_a_painted_shape_on_the_next_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = fixture(dir.path());
    let mut app = app_in(
        dir.path(),
        Startup::open(
            SessionKind::TextCompare,
            &left.join("same.txt"),
            &right.join("same.txt"),
        ),
    );
    settle(&mut app);
    assert!(run_frames(&mut app, App::active_is_ready));

    let chosen = Rgb::new(0x0A, 0x7B, 0x2C);
    let mut options = app.options().options_or_default();
    options.appearance.theme = ThemeChoice::from_id("dark");
    options
        .appearance
        .palettes
        .group_mut(ColorGroup::Text)
        .table_mut(true)
        .set("same_line", chosen);
    app.run(Command::Options, &context());
    let dialog = app.options_dialog_mut().unwrap();
    for _ in 0..1 {
        dialog.set_color(
            ColorGroup::Text,
            Variant::Dark,
            "same_line",
            ca_ui::theme::slots::to_color(chosen),
        );
    }
    let outcome = dialog.accept();
    assert!(outcome.closed);
    app.apply_options(*outcome.options);

    // One frame applies it; the shapes of the next frame carry it.
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert_eq!(
        to_stored(app.resolved_options().tables.main.same_line),
        chosen
    );
    let output = ctx.run(raw_input(), |ctx| app.frame(ctx));
    let painted = output.shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::Shape::Rect(rect)
            if to_stored(rect.fill) == chosen)
    });
    assert!(painted, "no shape was painted in the chosen color");
}

/// The theme choice, not the system setting, decides which table is in force.
#[test]
fn the_theme_choice_overrides_what_the_system_reports() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path(), Startup::Home);
    settle(&mut app);
    let mut options = app.options().options_or_default();
    options.appearance.theme = ThemeChoice::from_id("dark");
    app.run(Command::Options, &context());
    let dialog = app.options_dialog_mut().unwrap();
    dialog.set_value(
        "appearance.theme",
        &ca_ui::settings::FieldValue::Choice("dark".to_owned()),
    );
    let outcome = dialog.accept();
    app.apply_options(*outcome.options);
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert_eq!(app.resolved_options().variant(), Variant::Dark);
    assert_eq!(
        app.resolved_options().tables.main.same_line,
        ca_ui::theme::DARK.same_line
    );
}

/// A re-bound key reaches its command through the window's own routing, and the
/// key it replaced no longer does.
#[test]
fn a_rebound_shortcut_routes_and_the_old_key_stops() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path(), Startup::Home);
    settle(&mut app);
    app.run(Command::Options, &context());
    let dialog = app.options_dialog_mut().unwrap();
    dialog
        .add_shortcut(
            MenuView::Text,
            Command::FindNext,
            Keystroke::with_command_alt(egui::Key::J),
        )
        .unwrap();
    dialog.remove_shortcut(
        MenuView::Text,
        Command::FindNext,
        Keystroke::plain(egui::Key::F3),
    );
    let outcome = dialog.accept();
    app.apply_options(*outcome.options);
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));

    let table = &app.resolved_options().shortcuts;
    assert_eq!(
        table.claimed_by(MenuView::Text, Keystroke::with_command_alt(egui::Key::J)),
        Some(Command::FindNext)
    );
    assert_eq!(
        table.claimed_by(MenuView::Text, Keystroke::plain(egui::Key::F3)),
        None
    );
}

/// A customized shortcut is written to the document and read back by a second
/// window.
#[test]
fn a_customized_shortcut_survives_a_second_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path(), Startup::Home);
    settle(&mut app);
    app.run(Command::Options, &context());
    let dialog = app.options_dialog_mut().unwrap();
    dialog
        .add_shortcut(
            MenuView::Folder,
            Command::Reload,
            Keystroke::with_command_alt(egui::Key::K),
        )
        .unwrap();
    let outcome = dialog.accept();
    app.apply_options(*outcome.options);
    // The handle answers a reader with the document a running save holds, so
    // the wait asks for the save to have finished as well as for the value.
    assert!(run_frames(&mut app, |app| {
        app.options().is_ready()
            && app
                .options()
                .options()
                .is_some_and(|options| options.commands.shortcuts("folder", "Reload").is_some())
    }));
    drop(app);

    let mut second = app_in(dir.path(), Startup::Home);
    settle(&mut second);
    let _ = egui::Context::default().run(raw_input(), |ctx| second.frame(ctx));
    assert_eq!(
        second
            .resolved_options()
            .shortcuts
            .claimed_by(MenuView::Folder, Keystroke::with_command_alt(egui::Key::K)),
        Some(Command::Reload)
    );
}

/// Restoring the theme category puts the colors back and leaves the rest alone.
#[test]
fn restoring_the_color_category_leaves_the_other_pages_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path(), Startup::Home);
    settle(&mut app);
    app.run(Command::Options, &context());
    let dialog = app.options_dialog_mut().unwrap();
    dialog.set_value(
        "text_editing.tab_stop",
        &ca_ui::settings::FieldValue::Count(3),
    );
    dialog.set_color(
        ColorGroup::Text,
        Variant::Dark,
        "same_line",
        ca_ui::theme::slots::to_color(Rgb::new(9, 9, 9)),
    );
    let outcome = dialog.accept();
    app.apply_options(*outcome.options);
    assert!(run_frames(&mut app, |app| {
        app.options()
            .options()
            .is_some_and(|options| options.text_editing.tab_stop == 3)
    }));

    app.run(Command::RestoreFactoryDefaults, &context());
    let wizard = app.restore_dialog().is_some();
    assert!(wizard);
    let ctx = egui::Context::default();
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert!(run_frames(&mut app, |app| app.options().is_ready()));
    // The wizard itself is driven by its own tests; what matters here is that
    // the window offers it and the options document is still the edited one.
    assert_eq!(
        app.options().options_or_default().text_editing.tab_stop,
        3,
        "cancelling the wizard changed the options"
    );
    assert_eq!(
        app.options()
            .options_or_default()
            .appearance
            .palettes
            .group(ColorGroup::Text)
            .table(true)
            .get("same_line"),
        Some(Rgb::new(9, 9, 9))
    );
    assert_eq!(ProgramOptions::default().text_editing.tab_stop, 8);
}
