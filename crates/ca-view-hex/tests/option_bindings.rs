//! Real frames that check which application options the hex comparison reads.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_session::options::ProgramOptions;
use ca_session::AdminPolicies;
use ca_ui::command::Command;
use ca_ui::options::AppOptions;
use ca_ui::testing::{context, raw_input};
use ca_ui::theme::Variant;
use ca_ui::view::SessionView;
use ca_view_hex::HexView;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn a_save_keeps_a_copy_when_the_backup_page_asks_for_one() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.bin");
    let right = dir.path().join("right.bin");
    std::fs::write(&left, [1u8, 2, 3]).unwrap();
    std::fs::write(&right, [1u8, 2, 4]).unwrap();
    let view_context = context();
    let mut view = HexView::new(left, right, &view_context, 1);
    let mut options = ProgramOptions::default();
    options.backups.before_save = true;
    let resolved = Arc::new(AppOptions::resolve(
        options,
        Variant::Light,
        AdminPolicies::default(),
    ));
    let ctx = egui::Context::default();
    let frame = |view: &mut HexView| {
        view.tick();
        let _ = ctx.run(raw_input(), |ctx| {
            ca_ui::options::install(ctx, Arc::clone(&resolved));
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &view_context);
            });
        });
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while view.model().row_count() == 0 && Instant::now() < deadline {
        frame(&mut view);
        std::thread::sleep(Duration::from_millis(2));
    }
    view.run(Command::SaveFile);
    let copy = dir.path().join("left.bin.bak");
    while !copy.exists() && Instant::now() < deadline {
        frame(&mut view);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(std::fs::read(&copy).unwrap(), vec![1, 2, 3]);
}
