//! Real frames that check which application options a merge output save reads.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::options::ProgramOptions;
use ca_session::AdminPolicies;
use ca_ui::command::Command;
use ca_ui::options::AppOptions;
use ca_ui::testing::{context, sized_input};
use ca_ui::theme::Variant;
use ca_ui::view::{SessionView, Titles};
use ca_view_merge::jobs::MergePaths;
use ca_view_merge::MergeView;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Harness {
    view: MergeView,
    ctx: egui::Context,
    options: Arc<AppOptions>,
    dir: tempfile::TempDir,
}

impl Harness {
    /// A resolved merge over an output that already holds `old`.
    fn resolved(edit: impl FnOnce(&mut ProgramOptions)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let paths = MergePaths {
            left: write("left.txt", "a\nL\nc\n"),
            center: Some(write("center.txt", "a\nb\nc\n")),
            right: write("right.txt", "a\nR\nc\n"),
            output: Some(write("merged.txt", "old\n")),
        };
        let mut options = ProgramOptions::default();
        edit(&mut options);
        let mut harness = Self {
            view: MergeView::over(paths, Titles::default(), &context(), 11),
            ctx: egui::Context::default(),
            options: Arc::new(AppOptions::resolve(
                options,
                Variant::Light,
                AdminPolicies::default(),
            )),
            dir,
        };
        harness.until(SessionView::is_ready);
        harness.view.run(Command::NextConflict);
        harness.view.run(Command::TakeLeft);
        harness
    }

    fn frame(&mut self) {
        self.view.tick();
        let view = &mut self.view;
        let held = context();
        let options = Arc::clone(&self.options);
        let _ = self.ctx.run(sized_input(1_280.0, 800.0), |ctx| {
            ca_ui::options::install(ctx, Arc::clone(&options));
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &held);
            });
        });
    }

    fn until(&mut self, mut done: impl FnMut(&MergeView) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame();
            if done(&self.view) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the merge never reached the state");
    }

    fn save(&mut self) {
        self.view.run(Command::SaveFile);
        self.frame();
        self.until(|view| !view.is_saving());
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

#[test]
fn an_output_save_keeps_a_copy_of_the_old_output_when_the_page_asks_for_one() {
    let mut harness = Harness::resolved(|options| options.backups.before_save = true);
    harness.save();
    assert_eq!(
        std::fs::read_to_string(harness.path("merged.txt.bak")).unwrap(),
        "old\n"
    );
    assert_eq!(
        std::fs::read_to_string(harness.path("merged.txt")).unwrap(),
        "a\nL\nc\n"
    );
}

#[test]
fn an_output_save_writes_the_line_ending_the_page_names() {
    let mut harness = Harness::resolved(|options| {
        options.text_editing.line_endings_on_save =
            ca_session::options::LineEndingsOnSave::from_id("crlf");
    });
    harness.save();
    assert_eq!(
        std::fs::read(harness.path("merged.txt")).unwrap(),
        b"a\r\nL\r\nc\r\n"
    );
    assert!(!harness.path("merged.txt.bak").exists());
}
