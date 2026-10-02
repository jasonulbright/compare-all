//! Behaviour the window has to keep: a background tab still drains its work, a
//! refused command line reaches the user, and the interface fits a narrow
//! window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_app::cli::Startup;
use ca_app::App;
use ca_session::SessionKind;
use ca_ui::testing::{context, sized_input};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn raw_input(width: f32) -> egui::RawInput {
    sized_input(width, 800.0)
}

/// A window built from `startup`, over a settings directory of its own.
///
/// Tests of one binary run in parallel threads and the test binaries run one
/// after another over the same folder. A shared settings directory therefore
/// lets one test's claim on it, or one test's stored document, decide what
/// another test sees. The directory is removed when the returned value is
/// dropped, so it is bound beside the window that reads it.
fn started_app(startup: Startup) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let app = App::from_startup_in(
        startup,
        &context(),
        dir.path().to_path_buf(),
        dir.path().join("journals"),
    );
    (dir, app)
}

/// Characters in the path a planted journal names.
///
/// No window is wide enough for a path this long, so the notice that carries
/// it has to shorten it.
const LONG_PATH_CHARACTERS: usize = 300;

/// Write one journal that records a batch which never reported an end.
fn plant_journal(journals: &Path, target: &Path) -> PathBuf {
    std::fs::create_dir_all(journals).unwrap();
    let path = journals.join("batch-1-1-0.jsonl");
    let target = target.display().to_string().replace('\\', "\\\\");
    let body = format!(
        "{{\"record\":\"batch_start\",\"kind\":\"sync\",\"steps\":2,\"unix_seconds\":0}}\n\
         {{\"record\":\"step_begin\",\"index\":0,\"action\":{{\"copy_file\":\
         {{\"source\":\"{target}\",\"target\":\"{target}\"}}}},\"temporary\":null,\
         \"backup\":null}}\n"
    );
    std::fs::write(&path, body).unwrap();
    path
}

/// The area the text one frame painted covers, after clipping.
///
/// A widget that clips its own content does not draw outside the window, so
/// the clip rectangle is applied before the area is judged.
///
/// A label that runs past the window can leave the measured area alone, so the
/// shapes themselves are what a fit is judged by.
fn text_area(output: &egui::FullOutput) -> egui::Rect {
    let mut area = egui::Rect::NOTHING;
    for shape in &output.shapes {
        if let egui::Shape::Text(text) = &shape.shape {
            let bounds = text.visual_bounding_rect().intersect(shape.clip_rect);
            if bounds.is_finite() && bounds.is_positive() {
                area = area.union(bounds);
            }
        }
    }
    area
}

/// A path long enough that no window can hold it, under `root`.
fn long_path(root: &Path) -> PathBuf {
    let base = root.display().to_string().chars().count();
    let fill = LONG_PATH_CHARACTERS.saturating_sub(base + "/only-right.txt".len());
    root.join("d".repeat(fill)).join("only-right.txt")
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for side in ["left", "right"] {
        std::fs::create_dir_all(dir.path().join(side).join("sub")).unwrap();
    }
    std::fs::write(dir.path().join("left/same.txt"), b"alpha\nbeta\n").unwrap();
    std::fs::write(dir.path().join("right/same.txt"), b"alpha\nbeta\n").unwrap();
    std::fs::write(dir.path().join("left/sub/differs.txt"), b"one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("right/sub/differs.txt"), b"one\ntwo!\n").unwrap();
    std::fs::write(dir.path().join("left/orphan.txt"), b"only here\n").unwrap();
    dir
}

#[test]
fn a_background_tab_still_drains_its_work() {
    let dir = fixture();
    let (_settings, mut app) = started_app(Startup::open(
        SessionKind::FolderCompare,
        &dir.path().join("left"),
        &dir.path().join("right"),
    ));
    // A second tab takes the screen; the first is never painted again.
    app.open_home(&context());
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let _ = ctx.run(raw_input(1_280.0), |ctx| app.frame(ctx));
        if app.tab_is_ready(0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the background tab never finished its scan"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_refused_command_line_is_shown_in_the_window() {
    let (_settings, mut app) = started_app(Startup::Rejected("two paths or none".into()));
    assert_eq!(app.active_title().as_deref(), Some("Home"));
    assert_eq!(app.active_notice().as_deref(), Some("two paths or none"));
    // The window has no console behind it, so the banner is the only place the
    // reason appears; it has to survive being painted.
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let _ = ctx.run(raw_input(1_280.0), |ctx| app.frame(ctx));
    }
    assert_eq!(app.active_notice().as_deref(), Some("two paths or none"));
}

/// The toolbars have to fit a narrow window. Nothing a view draws may sit
/// outside the space the window gave it, on any frame.
#[test]
fn nothing_is_drawn_outside_a_narrow_window() {
    let dir = fixture();
    for width in [640.0_f32, 800.0, 1_280.0] {
        for startup in [
            Startup::Home,
            Startup::open(
                SessionKind::FolderCompare,
                &dir.path().join("left"),
                &dir.path().join("right"),
            ),
            Startup::open(
                SessionKind::TextCompare,
                &dir.path().join("left/sub/differs.txt"),
                &dir.path().join("right/sub/differs.txt"),
            ),
        ] {
            let (_settings, mut app) = started_app(startup);
            let ctx = egui::Context::default();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0));
            // The first frame is measured as well as the later ones: a toolbar
            // that fits only once it has been told what it needs would run past
            // the edge on the frame the window opens at.
            for frame in 0..8 {
                let _ = ctx.run(raw_input(width), |ctx| app.frame(ctx));
                let used = ctx.used_rect();
                // The very first frame reports nothing measured yet.
                if !used.is_finite() {
                    continue;
                }
                // A panel learns its height from the frame it drew, so the
                // panels below it take one frame to give way. The width is
                // decided before anything is placed and has to hold from the
                // first frame on.
                assert!(
                    used.left() >= screen.left() && used.right() <= screen.right(),
                    "at {width} points frame {frame} drew {used:?} outside {screen:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let used = ctx.used_rect();
            assert!(
                screen.contains_rect(used),
                "at {width} points the window settled at {used:?} outside {screen:?}"
            );
        }
    }
}

/// The startup check finds a journal whose steps name a path no window is wide
/// enough for. The notice that reports it still has to fit the window.
#[test]
fn a_recovery_notice_carrying_a_long_path_stays_inside_the_window() {
    let dir = fixture();
    let journals = dir.path().join("journals");
    let settings = dir.path().join("settings");
    let planted = long_path(dir.path());
    plant_journal(&journals, &planted);

    for width in [640.0_f32, 1_280.0] {
        let mut app = App::from_startup_in(
            Startup::Home,
            &context(),
            settings.clone(),
            journals.clone(),
        );
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0));
        let mut first = true;
        let mut raised = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let output = ctx.run(raw_input(width), |ctx| app.frame(ctx));
            let used = ctx.used_rect();
            let painted = text_area(&output);
            // The frame the window opens at has measured nothing yet.
            if !first {
                for area in [used, painted] {
                    if !area.is_finite() || !area.is_positive() {
                        continue;
                    }
                    // A one point stroke on the window edge is drawn centred on
                    // it, so half of it falls outside by design.
                    assert!(
                        area.left() >= screen.left() - 1.0 && area.right() <= screen.right() + 1.0,
                        "at {width} points the notice drew {area:?} outside {screen:?}"
                    );
                }
            }
            first = false;
            if raised {
                break;
            }
            if app
                .active_notice()
                .is_some_and(|notice| notice.contains("dddddddddd"))
            {
                // The notice is drawn on the frames that follow the one that
                // raised it, so one more frame is measured with it in place.
                raised = true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            raised,
            "at {width} points the unfinished batch was not reported"
        );
        let notice = app.active_notice().unwrap();
        assert!(
            notice.contains("dddddddddd"),
            "the notice does not name the batch's path: {notice}"
        );
    }
}

/// A batch whose files were removed after the interruption is still listed,
/// and the notice says the files are gone.
#[test]
fn a_batch_whose_files_are_gone_is_listed_and_says_so() {
    let dir = fixture();
    let journals = dir.path().join("journals");
    plant_journal(
        &journals,
        &dir.path().join("removed").join("only-right.txt"),
    );

    let mut job = ca_view_folder::recovery_scan_in(journals, std::sync::Arc::new(|| {}));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    while Instant::now() < deadline {
        for message in job.drain() {
            if let ca_view_folder::opjobs::RecoverMessage::Done { notices: found, .. } = message {
                notices = found;
            }
        }
        if !notices.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(notices.len(), 1, "the batch was dropped from the list");
    assert!(notices[0].files_missing);
    assert!(
        notices[0].summary.contains(ca_view_folder::MISSING_FILES),
        "{}",
        notices[0].summary
    );
}

/// No test may reach the real per-user settings folder.
#[test]
fn the_resolved_settings_directory_is_not_the_real_one() {
    ca_ui::testing::isolate_settings();
    assert!(!ca_ui::paths::is_real_per_user_location(
        &ca_ui::paths::settings_directory()
    ));
    assert!(!ca_ui::paths::is_real_per_user_location(
        &ca_ui::paths::journal_directory()
    ));
}

/// The window opens wide enough for two panes and refuses to be sized below
/// the point where it stops being two panes.
#[test]
#[allow(clippy::assertions_on_constants)]
fn the_window_has_a_default_and_a_minimum_size() {
    use ca_app::shell::{DEFAULT_WINDOW_SIZE, MINIMUM_WINDOW_SIZE};
    let default = egui::vec2(DEFAULT_WINDOW_SIZE[0], DEFAULT_WINDOW_SIZE[1]);
    let minimum = egui::vec2(MINIMUM_WINDOW_SIZE[0], MINIMUM_WINDOW_SIZE[1]);
    assert!((default - egui::vec2(1_280.0, 800.0)).length() < 0.001);
    assert!(minimum.x > 0.0 && minimum.y > 0.0);
    assert!(minimum.x < default.x && minimum.y < default.y);
}

/// Every kind the launcher offers has to open a tab, and a placeholder view has
/// to survive being painted in the window like any other.
#[test]
fn every_registered_kind_opens_and_paints() {
    let dir = fixture();
    let left = dir.path().join("left/sub/differs.txt");
    let right = dir.path().join("right/sub/differs.txt");
    for kind in ca_app::registry::available() {
        let (_settings, mut app) = started_app(Startup::open(kind.clone(), &left, &right));
        assert_eq!(app.tab_count(), 1, "{kind} opened no tab");
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let _ = ctx.run(raw_input(1_280.0), |ctx| app.frame(ctx));
        }
    }
}
