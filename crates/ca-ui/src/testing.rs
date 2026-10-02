//! Helpers for running real frames without a window.
//!
//! An egui context needs no graphics device to lay out and paint, so a test can
//! run whole frames of a view and assert on the model behind them. The helpers
//! here supply the input a window would and the context a view expects.

pub mod probe;

use crate::theme::{palette, Variant};
use crate::view::ViewContext;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The window size a frame is given when a test states none.
pub const DEFAULT_SCREEN: [f32; 2] = [1_280.0, 800.0];

/// Folder the isolated settings directory is placed in when the environment
/// names none.
const ISOLATED_FOLDER: &str = "compare-all-tests";

/// Point the settings directory at a folder of this process's own.
///
/// The variable is read on every path resolution, so setting it once before any
/// view is built keeps settings, journals and sessions away from the real
/// per-user folder for the rest of the process. An environment that already
/// names a directory is left alone.
pub fn isolate_settings() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let variable = ca_session::SETTINGS_DIRECTORY_VARIABLE;
        if let Some(named) = std::env::var_os(variable) {
            if !named.is_empty() {
                return PathBuf::from(named);
            }
        }
        let root = std::env::temp_dir()
            .join(ISOLATED_FOLDER)
            .join(std::process::id().to_string());
        std::env::set_var(variable, &root);
        root
    })
    .clone()
}

/// A settings directory of this process's own, created and empty of journals.
#[must_use]
pub fn settings_root() -> PathBuf {
    isolate_settings()
}

/// A view context over the light table whose repaint request does nothing.
#[must_use]
pub fn context() -> ViewContext {
    isolate_settings();
    ViewContext {
        options: Arc::default(),
        palette: palette(Variant::Light),
        notify: Arc::new(|| {}),
    }
}

/// A view context that counts the repaints its views ask for.
#[must_use]
pub fn counting_context(counter: Arc<std::sync::atomic::AtomicUsize>) -> ViewContext {
    isolate_settings();
    ViewContext {
        options: Arc::default(),
        palette: palette(Variant::Light),
        notify: Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    }
}

/// One frame of input over a window of the default size.
#[must_use]
pub fn raw_input() -> egui::RawInput {
    sized_input(DEFAULT_SCREEN[0], DEFAULT_SCREEN[1])
}

/// One frame of input over a window of the stated size.
#[must_use]
pub fn sized_input(width: f32, height: f32) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, height),
        )),
        ..egui::RawInput::default()
    }
}

/// One frame of input carrying `events` over a window of the stated size.
#[must_use]
pub fn event_input(width: f32, height: f32, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        events,
        ..sized_input(width, height)
    }
}

/// Poll `condition` until it holds or `budget` runs out, and report which.
pub fn wait_until(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Put a save that never ends in flight on both settings documents, for a
/// test of how long an exit waits for them.
pub fn stall_saves(
    store: &mut crate::sessions::StoreHandle,
    options: &mut crate::options::OptionsHandle,
) {
    store.stall_save();
    options.stall_save();
}
