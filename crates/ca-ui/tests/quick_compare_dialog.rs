//! The Quick Compare summary controls choose and open the next view.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::SessionKind;
use ca_ui::quick_compare::{QuickCompareAction, QuickCompareDialog};
use ca_ui::testing::probe::Probe;

fn draw<'a>(
    dialog: &'a mut QuickCompareDialog,
    action: &'a mut Option<QuickCompareAction>,
) -> impl FnMut(&egui::Context) + 'a {
    move |ctx| {
        if let Some(next) = dialog.show(ctx, egui::Id::new("quick-compare-test")) {
            *action = Some(next);
        }
    }
}

#[test]
fn the_result_can_open_the_chosen_view() {
    let mut dialog = QuickCompareDialog::new("left.txt", "right.txt");
    dialog.set_result("Important differences");
    let mut probe = Probe::new(900.0, 600.0);
    let mut action = None;
    let mut run = draw(&mut dialog, &mut action);
    probe.idle(&mut run);
    probe.idle(&mut run);
    assert!(probe.find("Open View").unwrap().enabled);

    probe.click("View", &mut run).unwrap();
    probe.idle(&mut run);
    probe.click("Hex Compare", &mut run).unwrap();
    probe.idle(&mut run);
    probe.click("Open View", &mut run).unwrap();
    drop(run);

    assert_eq!(
        action,
        Some(QuickCompareAction::OpenView(SessionKind::HexCompare))
    );
}

#[test]
fn closing_the_summary_returns_close() {
    let mut dialog = QuickCompareDialog::new("left.txt", "right.txt");
    let mut probe = Probe::new(900.0, 600.0);
    let mut action = None;
    let mut run = draw(&mut dialog, &mut action);
    probe.idle(&mut run);
    probe.idle(&mut run);
    probe.click("Close", &mut run).unwrap();
    drop(run);
    assert_eq!(action, Some(QuickCompareAction::Close));
}

#[test]
fn open_view_stays_disabled_until_the_result_is_ready() {
    let mut dialog = QuickCompareDialog::new("left.txt", "right.txt");
    let mut probe = Probe::new(900.0, 600.0);
    let mut action = None;
    let mut run = draw(&mut dialog, &mut action);
    probe.idle(&mut run);
    probe.idle(&mut run);

    assert!(!probe.find("Open View").unwrap().enabled);
    drop(run);
    assert_eq!(dialog.result(), None);
}

#[test]
fn the_window_close_control_returns_close() {
    let mut dialog = QuickCompareDialog::new("left.txt", "right.txt");
    let mut probe = Probe::new(900.0, 600.0);
    let mut action = None;
    let mut run = draw(&mut dialog, &mut action);
    probe.idle(&mut run);
    probe.idle(&mut run);
    probe.click("Close window", &mut run).unwrap();
    drop(run);

    assert_eq!(action, Some(QuickCompareAction::Close));
}
