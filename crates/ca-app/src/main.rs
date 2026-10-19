//! Window creation. Everything the window contains lives in the library.

#![cfg_attr(windows, windows_subsystem = "windows")]

use ca_app::cli;
use ca_app::renderer::{self, Outcome, Renderer, StartFailure};
use ca_app::shell::{DEFAULT_WINDOW_SIZE, MINIMUM_WINDOW_SIZE};
use ca_app::App;
use ca_ui::options::AppOptions;
use ca_ui::theme::{palette, Variant};
use ca_ui::view::ViewContext;
use std::sync::{Arc, Mutex};

/// Longest time the process waits at exit for the copies of closed tabs to go.
const EXIT_DELETE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

const VERSION_MARKER_TEXT: &str = concat!("CA-BUILD-VERSION=", env!("CA_VERSION"), "\0");

/// The build version inside a section of the program file, which the
/// signature covers. The updater refuses a downloaded copy whose version is
/// not the version of the release that names it.
#[used]
static VERSION_MARKER: [u8; VERSION_MARKER_TEXT.len()] = marker_bytes();

const fn marker_bytes() -> [u8; VERSION_MARKER_TEXT.len()] {
    let text = VERSION_MARKER_TEXT.as_bytes();
    let mut out = [0u8; VERSION_MARKER_TEXT.len()];
    let mut index = 0;
    while index < text.len() {
        out[index] = text[index];
        index += 1;
    }
    out
}

fn main() -> eframe::Result {
    std::hint::black_box(&VERSION_MARKER);
    // The update helper is a copy of this program started after the window
    // ended. It opens no window and reads no settings.
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // The elevated step of an update of an install for every user, started by
    // the helper through the elevation prompt. It opens no window and reads
    // no settings.
    if let Some(staging) = ca_update::install::parse_install_arguments(&arguments) {
        let Some(running) = ca_update::Version::parse(ca_app::VERSION) else {
            std::process::exit(ca_update::install::NOT_PREPARED);
        };
        let verifier = ca_update::signature::system_verifier();
        let code = ca_update::install::install_main(
            &staging,
            &ca_update::install::Checks {
                signer: verifier.as_ref(),
                signer_name: ca_update::signature::RELEASE_SIGNER,
                running: &running,
            },
        );
        std::process::exit(code);
    }
    if let Some(helper) = ca_update::apply::parse_helper_arguments(&arguments) {
        let Some(running) = ca_update::Version::parse(ca_app::VERSION) else {
            std::process::exit(1);
        };
        let verifier = ca_update::signature::system_verifier();
        let code = ca_update::apply::helper_main(
            &helper,
            verifier.as_ref(),
            ca_update::signature::RELEASE_SIGNER,
            &running,
        );
        std::process::exit(code);
    }
    install_panic_hook();
    // A version request is answered before the settings directory is
    // resolved, so it writes nothing and needs no display. On Windows this
    // binary has no console to print to, so the request stays a refused
    // command line shown in the launcher.
    #[cfg(not(windows))]
    if cli::asks_for_version(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        println!("{}", ca_app::VERSION);
        return Ok(());
    }
    // Portable mode is resolved before a window can start its first frame.
    let _ = ca_ui::paths::settings_directory();
    // A refused command line is carried into the launcher and shown there: on
    // Windows this binary has no console attached, so anything written to the
    // error stream goes nowhere.
    let stored = cli::stored_options().unwrap_or_default();
    let (preference, program_arguments) = renderer::take_preference(
        std::env::args_os().skip(1).collect(),
        std::env::var(renderer::ENVIRONMENT).ok().as_deref(),
    );
    let startup = cli::parse(program_arguments, &|path| {
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
    let update_exit = Arc::new(ca_app::update::ExitHook::default());
    let outcome = run_window(preference, startup, stored, &status, &update_exit);
    // A delete worker still running at process exit dies with the process and
    // leaves its copies behind until the sweep of the next start.
    ca_ui::view::wait_for_temporary_deletes(EXIT_DELETE_WAIT);
    ca_session::SettingsPaths::remove_run_directory();
    // A staged update is applied only after a normal close. A crash or a
    // forced end leaves it staged for the next normal close.
    ca_app::update::apply_at_exit(&update_exit, &ca_ui::paths::settings_directory());
    let code = status.load(std::sync::atomic::Ordering::SeqCst);
    if outcome.is_ok() && code != 0 {
        exit(code);
    }
    outcome
}

/// Start the window on each permitted back end in order and return how it ended.
fn run_window(
    preference: renderer::Preference,
    startup: cli::Startup,
    stored: ca_session::ProgramOptions,
    status: &Arc<std::sync::atomic::AtomicI32>,
    update_exit: &Arc<ca_app::update::ExitHook>,
) -> eframe::Result {
    let policy_load = ca_session::load_system_policies_reported();
    let policies = policy_load.policies;
    let policy_warning = policy_load.warning;
    // The first start attempt that reaches the application creator takes the
    // data; a later back end can start only when no earlier one got that far.
    let pending = Arc::new(Mutex::new(Some((startup, stored))));
    let mut window_error: Option<eframe::Error> = None;
    let outcome = renderer::start_in_order(preference, &mut |chosen| {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(DEFAULT_WINDOW_SIZE)
                .with_min_inner_size(MINIMUM_WINDOW_SIZE)
                .with_icon(Arc::new(ca_app::icon::window_icon())),
            renderer: match chosen {
                Renderer::Glow => eframe::Renderer::Glow,
                Renderer::Wgpu => eframe::Renderer::Wgpu,
            },
            ..eframe::NativeOptions::default()
        };
        let slot = Arc::clone(&pending);
        let reported = Arc::clone(status);
        let update_hook = Arc::clone(update_exit);
        let policy_warning = policy_warning.clone();
        let result = eframe::run_native(
            &ca_app::product_title(),
            options,
            Box::new(move |creation| {
                let taken = slot.lock().ok().and_then(|mut guard| guard.take());
                let Some((startup, stored)) = taken else {
                    return Err("the start data was already used".into());
                };
                renderer::record_active(chosen);
                eprintln!("{}", renderer::support_line());
                let repaint = creation.egui_ctx.clone();
                let variant = Variant::from_dark_mode(creation.egui_ctx.style().visuals.dark_mode);
                let context = ViewContext {
                    options: Arc::new(AppOptions::resolve(stored, variant, policies)),
                    palette: palette(variant),
                    notify: Arc::new(move || repaint.request_repaint()),
                };
                let mut app = App::from_startup(startup, &context);
                app.set_policies(policies);
                app.set_policy_warning(policy_warning);
                app.report_exit_to(reported);
                app.apply_updates_on_exit(update_hook);
                Ok(Box::new(app))
            }),
        );
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let failure = StartFailure {
                    message: error.to_string(),
                    application_ran: pending.lock().map_or(true, |guard| guard.is_none()),
                };
                window_error = Some(error);
                Err(failure)
            }
        }
    });
    match outcome {
        Outcome::Finished(_) => Ok(()),
        Outcome::Failed(..) => window_error.map_or(Ok(()), Err),
        Outcome::Unavailable(refusals) => {
            report_fatal(&renderer::unavailable_message(&refusals));
            exit(1)
        }
    }
}

/// Write `text` to the error stream and show it in a message box, which needs
/// no graphics driver.
fn report_fatal(text: &str) {
    eprintln!("{text}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Compare All")
        .set_description(text)
        .show();
}

/// A panic ends the process without unwinding, so the hook is the only place
/// that can tell the user why the window is gone.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        report_fatal(&renderer::panic_message(&info.to_string()));
    }));
}

/// End the process with `code`, deleting the private settings folder of this
/// run first, because the exit skips every destructor.
fn exit(code: i32) -> ! {
    ca_session::SettingsPaths::remove_run_directory();
    std::process::exit(code)
}
