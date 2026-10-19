//! A theme pack chosen in the options reaches the colors, the mode and the
//! painted icons on the next frames, and choosing the default puts them back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_app::cli::Startup;
use ca_app::App;
use ca_session::options::Rgb;
use ca_ui::testing::{context, raw_input};
use ca_ui::theme::slots::to_stored;
use ca_ui::theme::Variant;
use std::time::{Duration, Instant};

/// Run frames in `ctx` until `ready` holds or the budget runs out.
fn run_frames(app: &mut App, ctx: &egui::Context, ready: impl Fn(&App) -> bool) -> bool {
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

#[test]
fn a_chosen_theme_pack_applies_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let journals = dir.path().join("journals");
    let pack = settings.join("packs").join("sample");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::create_dir_all(&journals).unwrap();
    std::fs::write(
        pack.join("pack.json"),
        br##"{"format":1,"id":"sample","name":"Sample","version":"1","modes":["dark"],
        "colors":{"text":{"dark":{"slots":{"same_line":"#0A7B2C"}}}}}"##,
    )
    .unwrap();
    let mut app = App::from_startup_in(Startup::Home, &context(), settings, journals);
    let ctx = egui::Context::default();
    assert!(run_frames(&mut app, &ctx, |app| app.options().is_ready()));
    assert!(
        run_frames(&mut app, &ctx, |_| ca_ui::pack::catalog(&ctx)
            .find("sample")
            .is_some()),
        "the pack was never listed"
    );
    let before = app.resolved_options().tables.main.same_line;

    let mut options = app.options().options_or_default();
    options.appearance.theme_pack = "sample".to_owned();
    app.apply_options(options);
    assert!(
        run_frames(&mut app, &ctx, |_| ca_ui::pack::active(&ctx).is_some()),
        "the dark pack never took effect"
    );
    assert_eq!(
        to_stored(app.resolved_options().tables.main.same_line),
        Rgb::new(0x0A, 0x7B, 0x2C)
    );
    assert!(ca_ui::pack::active(&ctx).is_some());

    let mut options = app.options().options_or_default();
    options.appearance.theme_pack = String::new();
    app.apply_options(options);
    assert!(
        run_frames(&mut app, &ctx, |app| app
            .resolved_options()
            .tables
            .main
            .same_line
            == before),
        "the default look never came back"
    );
    assert!(ca_ui::pack::active(&ctx).is_none());
}

fn select(app: &mut App, id: &str) {
    let mut options = app.options().options_or_default();
    id.clone_into(&mut options.appearance.theme_pack);
    app.apply_options(options);
}

fn active_is(ctx: &egui::Context, id: &str) -> bool {
    ca_ui::pack::active(ctx).is_some_and(|pack| pack.id() == id)
}

#[test]
fn the_mode_follows_the_pack_and_returns_to_the_system_choice_when_the_pack_goes() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let journals = dir.path().join("journals");
    let packs = settings.join("packs");
    std::fs::create_dir_all(&journals).unwrap();
    for (name, modes) in [("darkonly", r#"["dark"]"#), ("both", r#"["light","dark"]"#)] {
        std::fs::create_dir_all(packs.join(name)).unwrap();
        std::fs::write(
            packs.join(name).join("pack.json"),
            format!(
                r#"{{"format":1,"id":"{name}","name":"{name}","version":"1","modes":{modes}}}"#
            ),
        )
        .unwrap();
    }
    let mut app = App::from_startup_in(Startup::Home, &context(), settings, journals);
    let ctx = egui::Context::default();
    ctx.set_theme(egui::Theme::Light);
    assert!(run_frames(&mut app, &ctx, |app| app.options().is_ready()));
    assert!(run_frames(&mut app, &ctx, |_| {
        let catalog = ca_ui::pack::catalog(&ctx);
        catalog.find("darkonly").is_some() && catalog.find("both").is_some()
    }));
    assert_eq!(app.resolved_options().variant(), Variant::Light);

    select(&mut app, "darkonly");
    assert!(run_frames(&mut app, &ctx, |_| active_is(&ctx, "darkonly")));
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert_eq!(app.resolved_options().variant(), Variant::Dark);
    assert!(ctx.style().visuals.dark_mode);

    select(&mut app, "both");
    assert!(run_frames(&mut app, &ctx, |_| active_is(&ctx, "both")));
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert_eq!(app.resolved_options().variant(), Variant::Light);

    select(&mut app, "darkonly");
    assert!(run_frames(&mut app, &ctx, |_| active_is(&ctx, "darkonly")));
    select(&mut app, "");
    assert!(run_frames(&mut app, &ctx, |_| ca_ui::pack::active(&ctx).is_none()));
    let _ = ctx.run(raw_input(), |ctx| app.frame(ctx));
    assert_eq!(app.resolved_options().variant(), Variant::Light);
}
