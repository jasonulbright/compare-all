//! Window creation. Everything the window contains lives in the library.

#![cfg_attr(windows, windows_subsystem = "windows")]

use ca_app::cli;
use ca_app::shell::{DEFAULT_WINDOW_SIZE, MINIMUM_WINDOW_SIZE};
use ca_app::App;
use ca_ui::options::AppOptions;
use ca_ui::theme::{palette, Variant};
use ca_ui::view::ViewContext;
use std::sync::Arc;

/// Longest time the process waits at exit for the copies of closed tabs to go.
const EXIT_DELETE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

fn main() -> eframe::Result {
    // Portable mode is resolved before a window can start its first frame.
    let _ = ca_ui::paths::settings_directory();
    // A refused command line is carried into the launcher and shown there: on
    // Windows this binary has no console attached, so anything written to the
    // error stream goes nowhere.
    let stored = cli::stored_options().unwrap_or_default();
    let startup = cli::parse(std::env::args_os().skip(1), &|path| {
        cli::probe_with(path, Some(&stored.archives))
    });
    // An automatic merge that finishes answers through its exit code alone and
    // never opens the window.
    let startup = match cli::run_automatic(startup) {
        Ok(startup) => startup,
        Err(code) => exit(code),
    };
    // The file manager menu starts the program once for each selected item, so
    // a comparison from it takes two starts: one remembers the left side.
    let probe = |path: &std::path::Path| cli::probe_with(path, Some(&stored.archives));
    let startup = match cli::run_left_side(startup, &ca_ui::paths::settings_directory(), &probe) {
        Ok(startup) => startup,
        Err(code) => exit(code),
    };
    // Copies of archive entries that an earlier run left behind, such as after
    // a crash, are deleted on a worker so the window never waits on the disk.
    std::thread::spawn(|| drop(ca_ui::paths::sweep_temporary()));
    // A merge started by another program answers through the exit code, so the
    // value has to outlive the window that decided it.
    let status = Arc::new(std::sync::atomic::AtomicI32::new(0));
    let reported = Arc::clone(&status);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(DEFAULT_WINDOW_SIZE)
            .with_min_inner_size(MINIMUM_WINDOW_SIZE)
            .with_icon(Arc::new(ca_app::icon::window_icon())),
        ..eframe::NativeOptions::default()
    };
    let outcome = eframe::run_native(
        &ca_app::product_title(),
        options,
        Box::new(move |creation| {
            let repaint = creation.egui_ctx.clone();
            let variant = Variant::from_dark_mode(creation.egui_ctx.style().visuals.dark_mode);
            let context = ViewContext {
                options: Arc::new(AppOptions::resolve(
                    stored,
                    variant,
                    ca_session::AdminPolicies::default(),
                )),
                palette: palette(variant),
                notify: Arc::new(move || repaint.request_repaint()),
            };
            let mut app = App::from_startup(startup, &context);
            app.report_exit_to(reported);
            Ok(Box::new(app))
        }),
    );
    // A delete worker still running at process exit dies with the process and
    // leaves its copies behind until the sweep of the next start.
    ca_ui::view::wait_for_temporary_deletes(EXIT_DELETE_WAIT);
    ca_session::SettingsPaths::remove_run_directory();
    let code = status.load(std::sync::atomic::Ordering::SeqCst);
    if outcome.is_ok() && code != 0 {
        exit(code);
    }
    outcome
}

/// End the process with `code`, deleting the private settings folder of this
/// run first, because the exit skips every destructor.
fn exit(code: i32) -> ! {
    ca_session::SettingsPaths::remove_run_directory();
    std::process::exit(code)
}
