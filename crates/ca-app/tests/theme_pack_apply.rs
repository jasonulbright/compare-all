//! A theme pack chosen in the options reaches the colors, the mode and the
//! painted icons on the next frames, and choosing the default puts them back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_app::cli::Startup;
use ca_app::App;
use ca_session::options::Rgb;
use ca_ui::testing::{context, raw_input};
use ca_ui::theme::slots::to_stored;
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
